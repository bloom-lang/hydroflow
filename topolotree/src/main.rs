#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::io;
use std::rc::Rc;
use std::time::Duration;

use futures::{SinkExt, Stream};
use hydroflow::bytes::{Bytes, BytesMut};
use hydroflow::hydroflow_syntax;
use hydroflow::scheduled::graph::Hydroflow;
use hydroflow::util::cli::{
    ConnectedDemux, ConnectedDirect, ConnectedSink, ConnectedSource, ConnectedTagged,
};

mod protocol;
use hydroflow::util::{deserialize_from_bytes, serialize_to_bytes};
use protocol::*;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NodeID(pub u32);

impl Display for NodeID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

type PostNeighborJoin = (((u64, Option<NodeID>), (i64, usize)), NodeID);

type ContributionAgg =
    Rc<RefCell<HashMap<u64, HashMap<Option<NodeID>, (Timestamped<i64>, usize)>>>>;

fn run_topolotree(
    neighbors: Vec<u32>,
    input_recv: impl Stream<Item = Result<(u32, BytesMut), io::Error>> + Unpin + 'static,
    increment_requests: impl Stream<Item = Result<BytesMut, io::Error>> + Unpin + 'static,
    output_send: tokio::sync::mpsc::UnboundedSender<(u32, Bytes)>,
    query_send: tokio::sync::mpsc::UnboundedSender<Bytes>,
) -> Hydroflow<'static> {
    fn merge(x: &mut i64, y: i64) {
        *x += y;
    }

    // Timestamp stuff is a bit complicated, there is a proper data-flowy way to do it
    // but it would require at least one more join and one more cross join just specifically for the local timestamps
    // Until we need it to be proper then we can take a shortcut and use rc refcell
    let self_timestamp = Rc::new(RefCell::new(HashMap::<u64, isize>::new()));

    let self_timestamp1 = Rc::clone(&self_timestamp);
    let self_timestamp2 = Rc::clone(&self_timestamp);
    let self_timestamp3 = Rc::clone(&self_timestamp);

    // we use current tick to keep track of which *keys* have been modified

    hydroflow_syntax! {
        parsed_input = source_stream(input_recv)
            -> map(Result::unwrap)
            -> map(|(src, x)| (NodeID(src), deserialize_from_bytes::<TopolotreeMessage>(&x).unwrap()))
            -> demux(|(src, msg), var_args!(payload, ping, pong)| {
                match msg {
                    TopolotreeMessage::Payload(p) => payload.give((src, p)),
                    TopolotreeMessage::Ping() => ping.give((src, ())),
                    TopolotreeMessage::Pong() => pong.give((src, ())),
                }
            });

        from_neighbors = parsed_input[payload] -> tee();
        pings = parsed_input[ping] -> tee();
        pongs = parsed_input[pong] -> tee();

        pings -> map(|(src, _)| (src, TopolotreeMessage::Pong())) -> output;

        // generate a ping every second
        neighbors -> [0]ping_generator;
        source_interval(Duration::from_secs(1)) -> [1]ping_generator;
        ping_generator = cross_join_multiset()
            -> map(|(src, _)| (src, TopolotreeMessage::Ping()))
            -> output;

        pongs -> dead_neighbors;
        pings -> dead_neighbors;
        new_neighbors -> map(|neighbor| (neighbor, ())) -> dead_neighbors; // fake pong
        dead_neighbors = union() -> fold_keyed::<'static>(Instant::now, |acc: &mut Instant, _| {
                *acc = Instant::now();
            })
            -> filter_map(|(node_id, acc)| {
                if acc.elapsed().as_secs() > 5 {
                    Some(node_id)
                } else {
                    None
                }
            }) -> tee();

        from_neighbors
            -> map(|(_, payload): (NodeID, Payload<i64>)| payload.key)
            -> touched_keys;

        operations
            -> map(|op| op.key)
            -> touched_keys;

        touched_keys = union() -> unique() -> [0]from_neighbors_unfiltered;

        from_neighbors
            -> map(|(src, payload): (NodeID, Payload<i64>)| (src, (payload.key, payload.contents)))
            -> fold::<'static>(|| Rc::new(RefCell::new(HashMap::new())), |acc: &mut ContributionAgg, (source, (key, val)): (NodeID, (u64, Timestamped<i64>))| {
                let mut acc = acc.borrow_mut();
                let key_entry = acc.entry(key).or_default();
                let src_entry = key_entry.entry(Some(source)).or_insert((Timestamped { timestamp: -1, data: 0 }, 0));
                if val.timestamp > src_entry.0.timestamp {
                    src_entry.0 = val;
                    *self_timestamp1.borrow_mut().entry(key).or_insert(0) += 1;
                    src_entry.1 = context.current_tick();
                }
            })
            -> from_neighbors_to_filter;

        from_neighbors_to_filter = union() -> [1]from_neighbors_unfiltered;
        from_neighbors_unfiltered =
            cross_join() ->
            flat_map(|(key, hashmap)| {
                let hashmap = hashmap.borrow();
                hashmap.get(&key).iter().flat_map(|v| v.iter()).map(|t| ((key, *t.0), (t.1.0.data, t.1.1))).collect::<Vec<_>>().into_iter()
            }) ->
            from_neighbors_or_local;

        operations = source_stream(increment_requests)
            -> map(|x| deserialize_from_bytes::<OperationPayload>(&x.unwrap()).unwrap())
            -> tee();
        local_values = operations
            -> inspect(|change| {
                *self_timestamp2.borrow_mut().entry(change.key).or_insert(0) += 1;
            })
            -> map(|change_payload: OperationPayload| (change_payload.key, (change_payload.change, context.current_tick())))
            -> fold::<'static>(|| Rc::new(RefCell::new(HashMap::new())), |agg: &mut ContributionAgg, change: (u64, (i64, usize))| {
                let mut agg = agg.borrow_mut();
                let agg_key = agg.entry(change.0).or_default();
                let agg_key = agg_key.entry(None).or_insert((Timestamped { timestamp: 0, data: 0 }, 0));

                agg_key.0.data += change.1.0;
                agg_key.1 = change.1.1;
            });

        local_values -> from_neighbors_to_filter;

        from_neighbors_or_local = tee();
        from_neighbors_or_local -> [0]all_neighbor_data;

        new_neighbors = source_iter(neighbors)
            -> map(NodeID)
            -> tee();

        new_neighbors
            -> persist()
            -> [pos]neighbors;
        dead_neighbors -> [neg]neighbors;
        neighbors = difference()
            -> tee();

        neighbors -> [1]all_neighbor_data;

        query_result = from_neighbors_or_local
            -> map(|((key, _), payload): ((u64, _), (i64, usize))| {
                (key, payload)
            })
            -> reduce_keyed(|acc: &mut (i64, usize), (data, change_tick): (i64, usize)| {
                merge(&mut acc.0, data);
                acc.1 = std::cmp::max(acc.1, change_tick);
            })
            -> filter(|(_, (_, change_tick))| *change_tick == context.current_tick())
            -> for_each(|(key, (data, _))| {
                let serialized = serialize_to_bytes(QueryResponse {
                    key,
                    value: data
                });
                query_send.send(serialized).unwrap();
            });

        all_neighbor_data = cross_join_multiset()
            -> filter(|(((_, aggregate_from_this_guy), _), target_neighbor): &PostNeighborJoin| {
                aggregate_from_this_guy.iter().all(|source| source != target_neighbor)
            })
            -> map(|(((key, _), payload), target_neighbor)| {
                ((key, target_neighbor), payload)
            })
            -> reduce_keyed(|acc: &mut (i64, usize), (data, change_tick): (i64, usize)| {
                merge(&mut acc.0, data);
                acc.1 = std::cmp::max(acc.1, change_tick);
            })
            -> filter(|(_, (_, change_tick))| *change_tick == context.current_tick())
            -> map(|((key, target_neighbor), (data, _))| (target_neighbor, Payload {
                key,
                contents: Timestamped {
                    timestamp: self_timestamp3.borrow().get(&key).copied().unwrap_or(0),
                    data,
                }
            }))
            -> map(|(target_neighbor, payload)| (target_neighbor, TopolotreeMessage::Payload(payload)))
            -> output;

        output = union() -> for_each(|(target_neighbor, output): (NodeID, TopolotreeMessage)| {
            let serialized = serialize_to_bytes(output);
            output_send.send((target_neighbor.0, serialized)).unwrap();
        });
    }
}

#[hydroflow::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let _self_id: u32 = args.next().unwrap().parse().unwrap();
    let neighbors: Vec<u32> = args.map(|x| x.parse().unwrap()).collect();

    let mut ports = hydroflow::util::cli::init().await;

    let input_recv = ports
        .port("from_peer")
        // connect to the port with a single recipient
        .connect::<ConnectedTagged<ConnectedDirect>>()
        .await
        .into_source();

    let mut output_send = ports
        .port("to_peer")
        .connect::<ConnectedDemux<ConnectedDirect>>()
        .await
        .into_sink();

    let operations_send = ports
        .port("increment_requests")
        // connect to the port with a single recipient
        .connect::<ConnectedDirect>()
        .await
        .into_source();

    let mut query_responses = ports
        .port("query_responses")
        .connect::<ConnectedDirect>()
        .await
        .into_sink();

    let (chan_tx, mut chan_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::task::spawn_local(async move {
        while let Some(msg) = chan_rx.recv().await {
            output_send.feed(msg).await.unwrap();
            while let Ok(msg) = chan_rx.try_recv() {
                output_send.feed(msg).await.unwrap();
            }
            output_send.flush().await.unwrap();
        }
    });

    let (query_tx, mut query_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        while let Some(msg) = query_rx.recv().await {
            query_responses.feed(msg).await.unwrap();
            while let Ok(msg) = query_rx.try_recv() {
                query_responses.feed(msg).await.unwrap();
            }
            query_responses.flush().await.unwrap();
        }
    });

    let flow = run_topolotree(neighbors, input_recv, operations_send, chan_tx, query_tx);

    let f1 = async move {
        #[cfg(target_os = "linux")]
        loop {
            let x = procinfo::pid::stat_self().unwrap();
            let bytes = x.rss * 1024 * 4;
            println!("memory,{}", bytes);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };

    // initial memory
    #[cfg(target_os = "linux")]
    {
        let x = procinfo::pid::stat_self().unwrap();
        let bytes = x.rss * 1024 * 4;
        println!("memory,{}", bytes);
    }

    let f1_handle = tokio::spawn(f1);
    hydroflow::util::cli::launch_flow(flow).await;
    f1_handle.abort();
}

use std::collections::HashMap;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_channel::{Receiver, Sender};
use async_process::{Command, Stdio};
use async_trait::async_trait;
use futures::io::BufReader;
use futures::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, StreamExt};
use hydroflow_cli_integration::ServerBindConfig;
use tokio::sync::RwLock;

use super::{
    ClientStrategy, Host, HostTargetType, LaunchedBinary, LaunchedHost, ResourceBatch,
    ResourceResult, ServerStrategy,
};

struct LaunchedLocalhostBinary {
    child: RwLock<async_process::Child>,
    stdin_sender: Sender<String>,
    stdout_cli_receivers: Arc<RwLock<Vec<Sender<String>>>>,
    stdout_receivers: Arc<RwLock<Vec<Sender<String>>>>,
    stderr_receivers: Arc<RwLock<Vec<Sender<String>>>>,
}

#[async_trait]
impl LaunchedBinary for LaunchedLocalhostBinary {
    async fn stdin(&self) -> Sender<String> {
        self.stdin_sender.clone()
    }

    async fn cli_stdout(&self) -> Receiver<String> {
        let mut receivers = self.stdout_cli_receivers.write().await;
        let (sender, receiver) = async_channel::unbounded::<String>();
        receivers.push(sender);
        receiver
    }

    async fn stdout(&self) -> Receiver<String> {
        let mut receivers = self.stdout_receivers.write().await;
        let (sender, receiver) = async_channel::unbounded::<String>();
        receivers.push(sender);
        receiver
    }

    async fn stderr(&self) -> Receiver<String> {
        let mut receivers = self.stderr_receivers.write().await;
        let (sender, receiver) = async_channel::unbounded::<String>();
        receivers.push(sender);
        receiver
    }

    async fn exit_code(&self) -> Option<i32> {
        self.child
            .write()
            .await
            .try_status()
            .ok()
            .flatten()
            .and_then(|c| {
                #[cfg(unix)]
                return c.code().or(c.signal());
                #[cfg(not(unix))]
                return c.code();
            })
    }

    async fn wait(&mut self) -> Option<i32> {
        let _ = self.child.get_mut().status().await;
        self.exit_code().await
    }
}

struct LaunchedLocalhost {}

type CLIAndMain = (
    Arc<RwLock<Vec<Sender<String>>>>,
    Arc<RwLock<Vec<Sender<String>>>>,
);

pub fn create_broadcast<T: AsyncRead + Send + Unpin + 'static>(
    source: T,
    default: impl Fn(String) + Send + 'static,
) -> CLIAndMain {
    let cli_receivers = Arc::new(RwLock::new(Vec::<Sender<String>>::new()));
    let receivers = Arc::new(RwLock::new(Vec::<Sender<String>>::new()));

    let weak_cli_receivers = Arc::downgrade(&cli_receivers);
    let weak_receivers = Arc::downgrade(&receivers);

    tokio::spawn(async move {
        let mut lines = BufReader::new(source).lines();

        'line_loop: while let Some(Result::Ok(line)) = lines.next().await {
            if let Some(cli_receivers) = weak_cli_receivers.upgrade() {
                let mut cli_receivers = cli_receivers.write().await;
                let mut successful_send = false;
                for r in cli_receivers.iter() {
                    successful_send |= r.send(line.clone()).await.is_ok();
                }

                cli_receivers.retain(|r| !r.is_closed());

                if successful_send {
                    continue 'line_loop;
                }
            }

            if let Some(receivers) = weak_receivers.upgrade() {
                let mut receivers = receivers.write().await;
                let mut successful_send = false;
                for r in receivers.iter() {
                    successful_send |= r.send(line.clone()).await.is_ok();
                }

                receivers.retain(|r| !r.is_closed());

                if !successful_send {
                    default(line);
                }
            } else {
                break;
            }
        }

        if let Some(cli_receivers) = weak_cli_receivers.upgrade() {
            let cli_receivers = cli_receivers.write().await;
            for r in cli_receivers.iter() {
                r.close();
            }
        }

        if let Some(receivers) = weak_receivers.upgrade() {
            let receivers = receivers.write().await;
            for r in receivers.iter() {
                r.close();
            }
        }
    });

    (cli_receivers, receivers)
}

#[async_trait]
impl LaunchedHost for LaunchedLocalhost {
    fn server_config(&self, bind_type: &ServerStrategy) -> ServerBindConfig {
        match bind_type {
            ServerStrategy::UnixSocket => ServerBindConfig::UnixSocket,
            ServerStrategy::InternalTcpPort => ServerBindConfig::TcpPort("127.0.0.1".to_string()),
            ServerStrategy::ExternalTcpPort(_) => panic!("Cannot bind to external port"),
            ServerStrategy::Demux(demux) => {
                let mut config_map = HashMap::new();
                for (key, underlying) in demux {
                    config_map.insert(*key, self.server_config(underlying));
                }

                ServerBindConfig::Demux(config_map)
            }
            ServerStrategy::Merge(merge) => {
                let mut configs = vec![];
                for underlying in merge {
                    configs.push(self.server_config(underlying));
                }

                ServerBindConfig::Merge(configs)
            }
            ServerStrategy::Tagged(underlying, id) => {
                ServerBindConfig::Tagged(Box::new(self.server_config(underlying)), *id)
            }
            ServerStrategy::Null => ServerBindConfig::Null,
        }
    }

    async fn copy_binary(&self, _binary: Arc<(String, Vec<u8>, PathBuf)>) -> Result<()> {
        Ok(())
    }

    async fn launch_binary(
        &self,
        id: String,
        binary: Arc<(String, Vec<u8>, PathBuf)>,
        args: &[String],
    ) -> Result<Arc<RwLock<dyn LaunchedBinary>>> {
        let mut child = Command::new(&binary.2)
            .args(args)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (stdin_sender, mut stdin_receiver) = async_channel::unbounded::<String>();
        let mut stdin = child.stdin.take().unwrap();
        tokio::spawn(async move {
            while let Some(line) = stdin_receiver.next().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }

                stdin.flush().await.ok();
            }
        });

        let id_clone = id.clone();
        let (stdout_cli_receivers, stdout_receivers) =
            create_broadcast(child.stdout.take().unwrap(), move |s| {
                println!("[{id_clone}] {s}")
            });
        let (_, stderr_receivers) = create_broadcast(child.stderr.take().unwrap(), move |s| {
            eprintln!("[{id}] {s}")
        });

        Ok(Arc::new(RwLock::new(LaunchedLocalhostBinary {
            child: RwLock::new(child),
            stdin_sender,
            stdout_cli_receivers,
            stdout_receivers,
            stderr_receivers,
        })))
    }

    async fn forward_port(&self, addr: &SocketAddr) -> Result<SocketAddr> {
        Ok(*addr)
    }
}

#[derive(Debug)]
pub struct LocalhostHost {
    pub id: usize,
    client_only: bool,
}

impl LocalhostHost {
    pub fn new(id: usize) -> LocalhostHost {
        LocalhostHost {
            id,
            client_only: false,
        }
    }

    pub fn client_only(&self) -> LocalhostHost {
        LocalhostHost {
            id: self.id,
            client_only: true,
        }
    }
}

#[async_trait]
impl Host for LocalhostHost {
    fn target_type(&self) -> HostTargetType {
        HostTargetType::Local
    }

    fn request_port(&mut self, _bind_type: &ServerStrategy) {}
    fn collect_resources(&self, _resource_batch: &mut ResourceBatch) {}
    fn request_custom_binary(&mut self) {}

    fn id(&self) -> usize {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn launched(&self) -> Option<Arc<dyn LaunchedHost>> {
        Some(Arc::new(LaunchedLocalhost {}))
    }

    async fn provision(&mut self, _resource_result: &Arc<ResourceResult>) -> Arc<dyn LaunchedHost> {
        Arc::new(LaunchedLocalhost {})
    }

    fn strategy_as_server<'a>(
        &'a self,
        connection_from: &dyn Host,
    ) -> Result<(
        ClientStrategy<'a>,
        Box<dyn FnOnce(&mut dyn std::any::Any) -> ServerStrategy>,
    )> {
        if self.client_only {
            anyhow::bail!("Localhost cannot be a server if it is client only")
        }

        if connection_from.can_connect_to(ClientStrategy::UnixSocket(self.id)) {
            Ok((
                ClientStrategy::UnixSocket(self.id),
                Box::new(|_| ServerStrategy::UnixSocket),
            ))
        } else if connection_from.can_connect_to(ClientStrategy::InternalTcpPort(self)) {
            Ok((
                ClientStrategy::InternalTcpPort(self),
                Box::new(|_| ServerStrategy::InternalTcpPort),
            ))
        } else {
            anyhow::bail!("Could not find a strategy to connect to localhost")
        }
    }

    fn can_connect_to(&self, typ: ClientStrategy) -> bool {
        match typ {
            ClientStrategy::UnixSocket(id) => {
                #[cfg(unix)]
                {
                    self.id == id
                }

                #[cfg(not(unix))]
                {
                    let _ = id;
                    false
                }
            }
            ClientStrategy::InternalTcpPort(target_host) => self.id == target_host.id(),
            ClientStrategy::ForwardedTcpPort(_) => true,
        }
    }
}

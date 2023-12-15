use hydroflow::hydroflow_syntax;

#[allow(non_snake_case)]
pub fn main() {
    let (R_send, R_recv) = hydroflow::util::unbounded_channel::<((usize, usize, usize), i32)>();
    let (T_send, T_recv) = hydroflow::util::unbounded_channel::<((usize, usize, usize), i32)>();
    let mut df = hydroflow_syntax! {
        // imports
        delta_join = import!("delta_set_join_static.hf");
        delta_distinct = import!("delta_distinct_static.hf");

        source_stream(T_recv)
            -> filter(|((_, a, _), _)| a > &2)
            -> map(|((id, _, x), n)| ((id, (x)), n)) // ((key, tup), mult)
            -> [0]delta_join;
        source_stream(R_recv)
            -> filter(|((_, s, _), _)| s > &5)
            -> map(|((id, _, y), m)| ((id, (y)), m)) // ((key, tup), mult)
            -> [1]delta_join;
        delta_join
            -> delta_distinct
            -> for_each(|t| println!("==> Output delta: {:?}", t));
    };
    df.run_available();

    // println!("\nStart tick 1");
    // one new join tuple. delta: (0, 6, 10, 1)
    R_send.send(((0, 9, 10), 1)).unwrap();
    R_send.send(((1, 0, 1), 1)).unwrap();
    T_send.send(((0, 5, 6), 1)).unwrap();
    T_send.send(((1, 0, 1), 1)).unwrap();
    df.run_tick();

    // println!("\nStart tick 2");
    // 3 new, 1 old, no new output. cumu: (0, 6, 10, 4)
    R_send.send(((0, 9, 10), 1)).unwrap(); // +1x1
    R_send.send(((1, 0, 1), 1)).unwrap();
    T_send.send(((0, 5, 6), 1)).unwrap(); // +1x2
    T_send.send(((1, 0, 1), 1)).unwrap();
    df.run_tick();

    // println!("\nStart tick 3");
    // -3 + 2 new, 4 old, no new output. cumu: (0, 6, 10, 3)
    R_send.send(((0, 9, 10), -1)).unwrap(); // -1x2
    R_send.send(((1, 0, 1), 1)).unwrap();
    T_send.send(((0, 5, 6), 1)).unwrap(); // +1x2 + -1x1
    T_send.send(((1, 0, 1), 1)).unwrap();
    df.run_tick();

    // println!("\nStart tick 4");
    // -5 + 2 new, 3 old, deletion output. cumu: (0, 6, 10, 0))
    R_send.send(((0, 9, 10), -1)).unwrap(); // -1x3
    R_send.send(((1, 0, 1), 1)).unwrap();
    T_send.send(((0, 5, 6), 1)).unwrap(); // +1x2 + -1x2
    T_send.send(((1, 0, 1), 1)).unwrap();
    df.run_tick();

    // println!(
    //     "{}",
    //     df.meta_graph()
    //         .expect("No graph found, maybe failed to parse.")
    //         .to_mermaid(&Default::default())
    // );
}

#[test]
fn test() {
    use hydroflow::util::{run_cargo_example, wait_for_process_output};

    let (_child, _, mut stdout) = run_cargo_example("dbsp", "");

    let mut output = String::new();
    // wait_for_process_output(&mut output, &mut stdout, "delta: ((0, 6, 10), 1)");
    // wait_for_process_output(
    //     &mut output,
    //     &mut stdout,
    //     r#"==> Output delta: ((0, 6, 10), 1)"#,
    // );
    // wait_for_process_output(
    //     &mut output,
    //     &mut stdout,
    //     r#"last cumulative: ((0, 6, 10), 1)"#,
    // );
    // wait_for_process_output(&mut output, &mut stdout, r#"delta: ((0, 6, 10), 3)"#);
    // wait_for_process_output(
    //     &mut output,
    //     &mut stdout,
    //     r#"last cumulative: ((0, 6, 10), 4)"#,
    // );
    // wait_for_process_output(&mut output, &mut stdout, r#"delta: ((0, 6, 10), -1)"#);
    // wait_for_process_output(
    //     &mut output,
    //     &mut stdout,
    //     r#"last cumulative: ((0, 6, 10), 3)"#,
    // );
    // wait_for_process_output(&mut output, &mut stdout, r#"delta: ((0, 6, 10), -3)"#);
    // wait_for_process_output(
    //     &mut output,
    //     &mut stdout,
    //     r#"==> Output delta: ((0, 6, 10), -1)"#,
    // );
}

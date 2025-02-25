use quote::quote_spanned;
use syn::parse_quote_spanned;

use super::{
    OperatorCategory, OperatorConstraints, WriteContextArgs,
    RANGE_0, RANGE_1,
};
use crate::graph::OperatorInstance;

/// > 1 input stream, 1 optional output stream
/// > Arguments: A Vector, Slice, or Array containing objects that will be compared to the input stream.
///
/// The input stream will be compared with the provided argument, element by element. If any elements do not match, `assert_eq` will panic.
/// If the input stream produces more elements than are in the provided argument, `assert_eq` will panic.
///
/// The input stream is passed through `assert_eq` unchanged to the output stream.
///
/// `assert_eq` is mainly useful for testing and documenting the behavior of hydroflow code inline.
///
/// `assert_eq` will remember the stream position across ticks, see example.
///
/// ```hydroflow
/// unioned = union();
///
/// source_iter([1]) -> assert_eq([1]) -> unioned;
/// source_iter([2]) -> defer_tick() -> assert_eq([2]) -> unioned;
///
/// unioned -> assert_eq([1, 2]);
/// ```
pub const ASSERT_EQ: OperatorConstraints = OperatorConstraints {
    name: "assert_eq",
    categories: &[OperatorCategory::Control],
    hard_range_inn: RANGE_1,
    soft_range_inn: RANGE_1,
    hard_range_out: &(0..=1),
    soft_range_out: &(0..=1),
    num_args: 1,
    persistence_args: RANGE_0,
    type_args: RANGE_0,
    is_external_input: false,
    ports_inn: None,
    ports_out: None,
    input_delaytype_fn: |_| None,
    flow_prop_fn: None,
    write_fn: |wc @ &WriteContextArgs {
                   context,
                   hydroflow,
                   op_span,
                   op_inst,
                   ..
               },
               diagnostics| {
        let assert_index_ident = wc.make_ident("assert_index");

        let args = &op_inst.arguments[0];

        let inspect_fn = parse_quote_spanned! {op_span=>
            |__item| {
                // This is to help constrain the types so that type inference works nicely.
                fn __constrain_types<T>(array: &impl ::std::ops::Index<usize, Output = T>, index: usize) -> &T {
                    &array[index]
                }

                let __index = #context.state_ref(#assert_index_ident).get();
                ::std::assert_eq!(__constrain_types(&#args, __index), __item, "Item (right) at index {} does not equal expected (left).", __index);
                #context.state_ref(#assert_index_ident).set(__index + 1);
            }
        };

        let wc = WriteContextArgs {
            op_inst: &OperatorInstance {
                arguments: inspect_fn,
                ..op_inst.clone()
            },
            ..wc.clone()
        };

        let mut owo = (super::inspect::INSPECT.write_fn)(&wc, diagnostics)?;

        let write_prologue = owo.write_prologue;
        owo.write_prologue = quote_spanned! {op_span=>
            let #assert_index_ident = #hydroflow.add_state(
                ::std::cell::Cell::new(0usize)
            );

            #write_prologue
        };

        Ok(owo)
    },
};

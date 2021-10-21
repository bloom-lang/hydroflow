use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/**
 * A trait specifying a handoff point between compiled subgraphs.
 */
pub trait Handoff {
    type Item;

    fn new() -> Self;

    #[allow(clippy::result_unit_err)]
    fn try_give(&mut self, item: Self::Item) -> Result<(), ()>;

    fn is_bottom(&self) -> bool;
}
/**
 * A handle onto the metadata part of a [Handoff], with no element type.
 */
pub trait HandoffMeta {
    // TODO(justin): more fine-grained info here.
    fn is_bottom(&self) -> bool;
}

/**
 * A null handoff which will panic when called.
 *
 * This is used in sources and sinks as the unused read or write handoff respectively.
 */
pub struct NullHandoff;
impl Handoff for NullHandoff {
    type Item = ();

    fn new() -> Self {
        NullHandoff
    }

    fn try_give(&mut self, _item: Self::Item) -> Result<(), ()> {
        panic!("Tried to write to null handoff.");
    }

    fn is_bottom(&self) -> bool {
        true
    }
}
impl HandoffMeta for NullHandoff {
    fn is_bottom(&self) -> bool {
        true
    }
}

/**
 * A [VecDeque]-based FIFO handoff.
 */
pub struct VecHandoff<T>(pub(crate) VecDeque<T>);
impl<T> Handoff for VecHandoff<T> {
    type Item = T;

    fn new() -> Self {
        VecHandoff(VecDeque::new())
    }

    fn try_give(&mut self, t: Self::Item) -> Result<(), ()> {
        self.0.push_back(t);
        Ok(())
    }

    fn is_bottom(&self) -> bool {
        self.0.is_empty()
    }
}
impl<H, T> HandoffMeta for Rc<RefCell<H>>
where
    H: Handoff<Item = T>,
{
    fn is_bottom(&self) -> bool {
        self.borrow().is_bottom()
    }
}

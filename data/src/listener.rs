//! Listener/observer traits.

pub trait Listener<T> {
    fn on_notify(&mut self, event: &T);
}

pub trait Producer<T> {
    fn add_listener(&mut self, listener: Box<dyn Listener<T>>);
    fn notify_listeners(&mut self, event: &T);
}

use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

#[derive(Clone)]
pub struct Shared<T: Clone>(Arc<T>);
impl<T: Clone> Shared<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}
impl<T: Clone> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

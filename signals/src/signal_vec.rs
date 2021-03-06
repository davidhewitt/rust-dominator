use std::cmp::Ordering;
use futures::{Stream, Poll, Async};
use futures::stream::ForEach;
use futures::future::IntoFuture;


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VecChange<A> {
    Replace {
        values: Vec<A>,
    },

    InsertAt {
        index: usize,
        value: A,
    },

    UpdateAt {
        index: usize,
        value: A,
    },

    RemoveAt {
        index: usize,
    },

    // TODO
    /*Swap {
        old_index: usize,
        new_index: usize,
    },*/

    Push {
        value: A,
    },

    Pop {},

    Clear {},
}

impl<A> VecChange<A> {
    // TODO inline this ?
    fn map<B, F>(self, mut callback: F) -> VecChange<B> where F: FnMut(A) -> B {
        match self {
            // TODO figure out a more efficient way of implementing this
            VecChange::Replace { values } => VecChange::Replace { values: values.into_iter().map(callback).collect() },
            VecChange::InsertAt { index, value } => VecChange::InsertAt { index, value: callback(value) },
            VecChange::UpdateAt { index, value } => VecChange::UpdateAt { index, value: callback(value) },
            VecChange::RemoveAt { index } => VecChange::RemoveAt { index },
            VecChange::Push { value } => VecChange::Push { value: callback(value) },
            VecChange::Pop {} => VecChange::Pop {},
            VecChange::Clear {} => VecChange::Clear {},
        }
    }
}


pub trait SignalVec {
    type Item;

    fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>>;

    #[inline]
    fn map<A, F>(self, callback: F) -> Map<Self, F>
        where F: FnMut(Self::Item) -> A,
              Self: Sized {
        Map {
            signal: self,
            callback,
        }
    }

    #[inline]
    fn filter_map<A, F>(self, callback: F) -> FilterMap<Self, F>
        where F: FnMut(Self::Item) -> Option<A>,
              Self: Sized {
        FilterMap {
            length: 0,
            indexes: vec![],
            signal: self,
            callback,
        }
    }

    #[inline]
    fn sort_by<F>(self, compare: F) -> SortBy<Self, F>
        where F: FnMut(&Self::Item, &Self::Item) -> Ordering,
              Self: Sized {
        SortBy {
            pending: None,
            values: vec![],
            indexes: vec![],
            signal: self,
            compare,
        }
    }

    #[inline]
    fn to_stream(self) -> SignalVecStream<Self> where Self: Sized {
        SignalVecStream {
            signal: self,
        }
    }

    #[inline]
    // TODO file Rust bug about bad error message when `callback` isn't marked as `mut`
    fn for_each<F, U>(self, callback: F) -> ForEach<SignalVecStream<Self>, F, U>
        where F: FnMut(VecChange<Self::Item>) -> U,
              // TODO allow for errors ?
              U: IntoFuture<Item = (), Error = ()>,
              Self:Sized {

        self.to_stream().for_each(callback)
    }

    #[inline]
    fn by_ref(&mut self) -> &mut Self {
        self
    }
}


pub struct Map<A, B> {
    signal: A,
    callback: B,
}

impl<A, B, F> SignalVec for Map<A, F>
    where A: SignalVec,
          F: FnMut(A::Item) -> B {
    type Item = B;

    // TODO should this inline ?
    #[inline]
    fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>> {
        self.signal.poll().map(|some| some.map(|change| change.map(|value| (self.callback)(value))))
    }
}


pub struct SignalVecStream<A> {
    signal: A,
}

impl<A: SignalVec> Stream for SignalVecStream<A> {
    type Item = VecChange<A::Item>;
    type Error = ();

    #[inline]
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.signal.poll() {
            Async::Ready(some) => Ok(Async::Ready(some)),
            Async::NotReady => Ok(Async::NotReady),
        }
    }
}


pub struct FilterMap<A, B> {
    length: usize,
    indexes: Vec<Option<usize>>,
    signal: A,
    callback: B,
}

impl<A, B> FilterMap<A, B> {
    fn increment_indexes(&mut self, index: usize) -> usize {
        let mut first = None;

        for index in &mut self.indexes[index..] {
            if let Some(i) = *index {
                if let None = first {
                    first = Some(i);
                }

                *index = Some(i + 1);
            }
        }

        first.unwrap_or(self.length)
    }

    fn decrement_indexes(&mut self, index: usize) {
        for index in &mut self.indexes[index..] {
            if let Some(i) = *index {
                *index = Some(i - 1);
            }
        }
    }
}

impl<A, B, F> SignalVec for FilterMap<A, F>
    where A: SignalVec,
          F: FnMut(A::Item) -> Option<B> {
    type Item = B;

    // TODO figure out a faster implementation of this
    fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>> {
        loop {
            return match self.signal.poll() {
                Async::NotReady => return Async::NotReady,
                Async::Ready(None) => return Async::Ready(None),
                Async::Ready(Some(change)) => match change {
                    VecChange::Replace { values } => {
                        self.length = 0;
                        self.indexes = Vec::with_capacity(values.len());

                        Async::Ready(Some(VecChange::Replace {
                            values: values.into_iter().filter_map(|value| {
                                let value = (self.callback)(value);

                                match value {
                                    Some(_) => {
                                        self.indexes.push(Some(self.length));
                                        self.length += 1;
                                    },
                                    None => {
                                        self.indexes.push(None);
                                    },
                                }

                                value
                            }).collect()
                        }))
                    },

                    VecChange::InsertAt { index, value } => {
                        match (self.callback)(value) {
                            Some(value) => {
                                let new_index = self.increment_indexes(index);

                                self.indexes.insert(index, Some(new_index));
                                self.length += 1;

                                Async::Ready(Some(VecChange::InsertAt { index: new_index, value }))
                            },
                            None => {
                                self.indexes.insert(index, None);
                                continue;
                            },
                        }
                    },

                    VecChange::UpdateAt { index, value } => {
                        match (self.callback)(value) {
                            Some(value) => {
                                match self.indexes[index] {
                                    Some(old_index) => {
                                        Async::Ready(Some(VecChange::UpdateAt { index: old_index, value }))
                                    },
                                    None => {
                                        let new_index = self.increment_indexes(index + 1);

                                        self.indexes[index] = Some(new_index);
                                        self.length += 1;

                                        Async::Ready(Some(VecChange::InsertAt { index: new_index, value }))
                                    },
                                }
                            },
                            None => {
                                match self.indexes[index] {
                                    Some(old_index) => {
                                        self.indexes[index] = None;

                                        self.decrement_indexes(index + 1);
                                        self.length -= 1;

                                        Async::Ready(Some(VecChange::RemoveAt { index: old_index }))
                                    },
                                    None => {
                                        continue;
                                    },
                                }
                            },
                        }
                    },

                    VecChange::RemoveAt { index } => {
                        match self.indexes.remove(index) {
                            Some(old_index) => {
                                self.decrement_indexes(index);
                                self.length -= 1;

                                Async::Ready(Some(VecChange::RemoveAt { index: old_index }))
                            },
                            None => {
                                continue;
                            },
                        }
                    },

                    VecChange::Push { value } => {
                        match (self.callback)(value) {
                            Some(value) => {
                                self.indexes.push(Some(self.length));
                                self.length += 1;
                                Async::Ready(Some(VecChange::Push { value }))
                            },
                            None => {
                                self.indexes.push(None);
                                continue;
                            },
                        }
                    },

                    VecChange::Pop {} => {
                        match self.indexes.pop().expect("Cannot pop from empty vec") {
                            Some(_) => {
                                Async::Ready(Some(VecChange::Pop {}))
                            },
                            None => {
                                continue;
                            },
                        }
                    },

                    VecChange::Clear {} => {
                        self.length = 0;
                        self.indexes = vec![];
                        Async::Ready(Some(VecChange::Clear {}))
                    },
                },
            }
        }
    }
}


pub struct SortBy<A: SignalVec, B> {
    pending: Option<Async<Option<VecChange<A::Item>>>>,
    values: Vec<A::Item>,
    indexes: Vec<usize>,
    signal: A,
    compare: B,
}

impl<A, F> SortBy<A, F>
    where A: SignalVec,
          F: FnMut(&A::Item, &A::Item) -> Ordering {
    // TODO should this inline ?
    fn binary_search(&mut self, index: usize) -> Result<usize, usize> {
        let compare = &mut self.compare;
        let values = &self.values;
        let value = &values[index];

        // TODO use get_unchecked ?
        self.indexes.binary_search_by(|i| compare(&values[*i], value).then_with(|| i.cmp(&index)))
    }

    fn binary_search_insert(&mut self, index: usize) -> usize {
        match self.binary_search(index) {
            Ok(_) => panic!("Value already exists"),
            Err(new_index) => new_index,
        }
    }

    fn binary_search_remove(&mut self, index: usize) -> usize {
        self.binary_search(index).expect("Could not find value")
    }

    fn increment_indexes(&mut self, start: usize) {
        for index in &mut self.indexes {
            let i = *index;

            if i >= start {
                *index = i + 1;
            }
        }
    }

    fn decrement_indexes(&mut self, start: usize) {
        for index in &mut self.indexes {
            let i = *index;

            if i > start {
                *index = i - 1;
            }
        }
    }

    fn insert_at(&mut self, sorted_index: usize, index: usize, value: A::Item) -> Async<Option<VecChange<A::Item>>> {
        if sorted_index == self.indexes.len() {
            self.indexes.push(index);

            Async::Ready(Some(VecChange::Push {
                value,
            }))

        } else {
            self.indexes.insert(sorted_index, index);

            Async::Ready(Some(VecChange::InsertAt {
                index: sorted_index,
                value,
            }))
        }
    }

    fn remove_at(&mut self, sorted_index: usize) -> Async<Option<VecChange<A::Item>>> {
        if sorted_index == (self.indexes.len() - 1) {
            self.indexes.pop();

            Async::Ready(Some(VecChange::Pop {}))

        } else {
            self.indexes.remove(sorted_index);

            Async::Ready(Some(VecChange::RemoveAt {
                index: sorted_index,
            }))
        }
    }
}

impl<A, F> SignalVec for SortBy<A, F>
    where A: SignalVec,
          F: FnMut(&A::Item, &A::Item) -> Ordering,
          A::Item: Clone {
    type Item = A::Item;

    // TODO figure out a faster implementation of this
    fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>> {
        match self.pending.take() {
            Some(value) => value,
            None => match self.signal.poll() {
                Async::NotReady => Async::NotReady,
                Async::Ready(None) => Async::Ready(None),
                Async::Ready(Some(change)) => match change {
                    VecChange::Replace { mut values } => {
                        // TODO can this be made faster ?
                        let mut indexes: Vec<usize> = (0..values.len()).collect();

                        // TODO use get_unchecked ?
                        indexes.sort_unstable_by(|a, b| (self.compare)(&values[*a], &values[*b]).then_with(|| a.cmp(b)));

                        let output = Async::Ready(Some(VecChange::Replace {
                            // TODO use get_unchecked ?
                            values: indexes.iter().map(|i| values[*i].clone()).collect()
                        }));

                        self.values = values;
                        self.indexes = indexes;

                        output
                    },

                    VecChange::InsertAt { index, value } => {
                        let new_value = value.clone();

                        self.values.insert(index, value);

                        self.increment_indexes(index);

                        let sorted_index = self.binary_search_insert(index);

                        self.insert_at(sorted_index, index, new_value)
                    },

                    VecChange::Push { value } => {
                        let new_value = value.clone();

                        let index = self.values.len();

                        self.values.push(value);

                        let sorted_index = self.binary_search_insert(index);

                        self.insert_at(sorted_index, index, new_value)
                    },

                    VecChange::UpdateAt { index, value } => {
                        let old_index = self.binary_search_remove(index);

                        let old_output = self.remove_at(old_index);

                        let new_value = value.clone();

                        self.values[index] = value;

                        let new_index = self.binary_search_insert(index);

                        if old_index == new_index {
                            self.indexes.insert(new_index, index);

                            Async::Ready(Some(VecChange::UpdateAt {
                                index: new_index,
                                value: new_value,
                            }))

                        } else {
                            let new_output = self.insert_at(new_index, index, new_value);
                            self.pending = Some(new_output);

                            old_output
                        }
                    },

                    VecChange::RemoveAt { index } => {
                        let sorted_index = self.binary_search_remove(index);

                        self.values.remove(index);

                        self.decrement_indexes(index);

                        self.remove_at(sorted_index)
                    },

                    VecChange::Pop {} => {
                        let index = self.values.len() - 1;

                        let sorted_index = self.binary_search_remove(index);

                        self.values.pop();

                        self.remove_at(sorted_index)
                    },

                    VecChange::Clear {} => {
                        self.values = vec![];
                        self.indexes = vec![];
                        Async::Ready(Some(VecChange::Clear {}))
                    },
                },
            },
        }
    }
}


// TODO verify that this is correct
pub mod unsync {
    use super::{SignalVec, VecChange};
    use futures::unsync::mpsc;
    use futures::{Async, Stream};


    pub struct Sender<A> {
        values: Vec<A>,
        sender: mpsc::UnboundedSender<VecChange<A>>,
    }

    impl<A: Clone> Sender<A> {
        pub fn push(&mut self, value: A) {
            let clone = value.clone();
            self.values.push(value);
            self.sender.unbounded_send(VecChange::Push { value: clone }).unwrap();
        }

        pub fn insert(&mut self, index: usize, value: A) {
            let clone = value.clone();

            if index == self.values.len() {
                self.values.push(value);
                self.sender.unbounded_send(VecChange::Push { value: clone }).unwrap();

            } else {
                self.values.insert(index, value);
                self.sender.unbounded_send(VecChange::InsertAt { index, value: clone }).unwrap();
            }
        }

        // TODO replace this with something else, like entry or IndexMut or whatever
        pub fn update(&mut self, index: usize, value: A) {
            let clone = value.clone();
            self.values[index] = value;
            self.sender.unbounded_send(VecChange::UpdateAt { index, value: clone }).unwrap();
        }
    }

    impl<A> Sender<A> {
        pub fn pop(&mut self) -> Option<A> {
            let value = self.values.pop();

            if let Some(_) = value {
                self.sender.unbounded_send(VecChange::Pop {}).unwrap();
            }

            value
        }

        pub fn remove(&mut self, index: usize) -> A {
            let len = self.values.len();

            let value = self.values.remove(index);

            if index == (len - 1) {
                self.sender.unbounded_send(VecChange::Pop {}).unwrap();

            } else {
                self.sender.unbounded_send(VecChange::RemoveAt { index }).unwrap();
            }

            value
        }

        pub fn clear(&mut self) {
            let len = self.values.len();

            self.values.clear();

            if len > 0 {
                self.sender.unbounded_send(VecChange::Clear {}).unwrap();
            }
        }
    }


    pub struct Receiver<A> {
        receiver: mpsc::UnboundedReceiver<VecChange<A>>,
    }

    // TODO have it send a Replace at the beginning
    impl<A> SignalVec for Receiver<A> {
        type Item = A;

        #[inline]
        fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>> {
            self.receiver.poll().unwrap()
        }
    }


    #[inline]
    pub fn mutable<A>() -> (Sender<A>, Receiver<A>) {
        let (sender, receiver) = mpsc::unbounded();

        let sender = Sender { values: vec![], sender };

        let receiver = Receiver { receiver };

        (sender, receiver)
    }
}


#[cfg(test)]
mod tests {
    use futures::{Future, Poll, task};
    use super::*;

    struct Tester<A> {
        changes: Vec<Async<VecChange<A>>>,
    }

    impl<A> Tester<A> {
        #[inline]
        fn new(changes: Vec<Async<VecChange<A>>>) -> Self {
            Self { changes }
        }
    }

    impl<A> SignalVec for Tester<A> {
        type Item = A;

        #[inline]
        fn poll(&mut self) -> Async<Option<VecChange<Self::Item>>> {
            if self.changes.len() > 0 {
                match self.changes.remove(0) {
                    Async::NotReady => {
                        task::current().notify();
                        Async::NotReady
                    },
                    Async::Ready(change) => Async::Ready(Some(change)),
                }

            } else {
                Async::Ready(None)
            }
        }
    }


    struct TesterFuture<A, B> {
        signal: A,
        callback: B,
    }

    impl<A: SignalVec, B: FnMut(&mut A, VecChange<A::Item>)> TesterFuture<A, B> {
        #[inline]
        fn new(signal: A, callback: B) -> Self {
            Self { signal, callback }
        }
    }

    impl<A, B> Future for TesterFuture<A, B>
        where A: SignalVec,
              B: FnMut(&mut A, VecChange<A::Item>) {

        type Item = ();
        type Error = ();

        #[inline]
        fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
            loop {
                return match self.signal.poll() {
                    Async::Ready(Some(change)) => {
                        (self.callback)(&mut self.signal, change);
                        continue;
                    },
                    Async::Ready(None) => Ok(Async::Ready(())),
                    Async::NotReady => Ok(Async::NotReady),
                }
            }
        }
    }

    fn run<A: SignalVec, B: FnMut(&mut A, VecChange<A::Item>) -> C, C>(signal: A, mut callback: B) -> Vec<C> {
        let mut changes = vec![];

        TesterFuture::new(signal, |signal, change| {
            changes.push(callback(signal, change));
        }).wait().unwrap();

        changes
    }


    #[test]
    fn filter_map() {
        #[derive(Debug, PartialEq, Eq)]
        struct Change {
            length: usize,
            indexes: Vec<Option<usize>>,
            change: VecChange<u32>,
        }

        let input = Tester::new(vec![
            Async::Ready(VecChange::Replace { values: vec![0, 1, 2, 3, 4, 5] }),
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 0, value: 6 }),
            Async::Ready(VecChange::InsertAt { index: 2, value: 7 }),
            Async::NotReady,
            Async::NotReady,
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 5, value: 8 }),
            Async::Ready(VecChange::InsertAt { index: 7, value: 9 }),
            Async::Ready(VecChange::InsertAt { index: 9, value: 10 }),
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 11, value: 11 }),
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 0, value: 0 }),
            Async::NotReady,
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 1, value: 0 }),
            Async::Ready(VecChange::InsertAt { index: 5, value: 0 }),
            Async::NotReady,
            Async::Ready(VecChange::InsertAt { index: 5, value: 12 }),
            Async::NotReady,
            Async::Ready(VecChange::RemoveAt { index: 0 }),
            Async::Ready(VecChange::RemoveAt { index: 0 }),
            Async::NotReady,
            Async::Ready(VecChange::RemoveAt { index: 0 }),
            Async::Ready(VecChange::RemoveAt { index: 1 }),
            Async::NotReady,
            Async::Ready(VecChange::RemoveAt { index: 0 }),
            Async::NotReady,
            Async::Ready(VecChange::RemoveAt { index: 0 }),
        ]);

        let output = input.filter_map(|x| {
            if x == 3 || x == 4 || x > 5 {
                Some(x + 100)
            } else {
                None
            }
        });

        assert_eq!(output.length, 0);
        assert_eq!(output.indexes, vec![]);

        let changes = run(output, |output, change| {
            Change {
                change: change,
                length: output.length,
                indexes: output.indexes.clone(),
            }
        });

        assert_eq!(changes, vec![
            Change { length: 2, indexes: vec![None, None, None, Some(0), Some(1), None], change: VecChange::Replace { values: vec![103, 104] } },
            Change { length: 3, indexes: vec![Some(0), None, None, None, Some(1), Some(2), None], change: VecChange::InsertAt { index: 0, value: 106 } },
            Change { length: 4, indexes: vec![Some(0), None, Some(1), None, None, Some(2), Some(3), None], change: VecChange::InsertAt { index: 1, value: 107 } },
            Change { length: 5, indexes: vec![Some(0), None, Some(1), None, None, Some(2), Some(3), Some(4), None], change: VecChange::InsertAt { index: 2, value: 108 } },
            Change { length: 6, indexes: vec![Some(0), None, Some(1), None, None, Some(2), Some(3), Some(4), Some(5), None], change: VecChange::InsertAt { index: 4, value: 109 } },
            Change { length: 7, indexes: vec![Some(0), None, Some(1), None, None, Some(2), Some(3), Some(4), Some(5), Some(6), None], change: VecChange::InsertAt { index: 6, value: 110 } },
            Change { length: 8, indexes: vec![Some(0), None, Some(1), None, None, Some(2), Some(3), Some(4), Some(5), Some(6), None, Some(7)], change: VecChange::InsertAt { index: 7, value: 111 } },
            Change { length: 9, indexes: vec![None, None, Some(0), None, Some(1), Some(2), None, None, None, Some(3), Some(4), Some(5), Some(6), Some(7), None, Some(8)], change: VecChange::InsertAt { index: 2, value: 112 } },
            Change { length: 8, indexes: vec![None, Some(0), Some(1), None, None, None, Some(2), Some(3), Some(4), Some(5), Some(6), None, Some(7)], change: VecChange::RemoveAt { index: 0 } },
            Change { length: 7, indexes: vec![None, Some(0), None, None, None, Some(1), Some(2), Some(3), Some(4), Some(5), None, Some(6)], change: VecChange::RemoveAt { index: 0 } },
            Change { length: 6, indexes: vec![None, None, None, Some(0), Some(1), Some(2), Some(3), Some(4), None, Some(5)], change: VecChange::RemoveAt { index: 0 } },
        ]);
    }
}

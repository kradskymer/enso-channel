/// Internal macros for keeping the public channel API consistent across
/// multiple topologies without duplicating wrapper code.
///
/// These macros are *crate implementation details*.
#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_send_batch {
	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident < $lt:lifetime, T, F > = ( $publisher_sequencer:ty );
	) => {
		$(#[$meta])*
		$vis struct $name<$lt, T, F>
		where
			F: Fn() -> T + Copy,
		{
			inner: $crate::permit::SendBatch<$lt, $publisher_sequencer, T, F>,
		}

		impl<T, F> $name<'_, T, F>
		where
			F: Fn() -> T + Copy,
		{
			/// Total number of items in the batch.
			pub fn capacity(&self) -> usize {
				self.inner.capacity()
			}

			/// Remaining items that must be written before the batch is full.
			pub fn remaining(&self) -> usize {
				self.inner.remaining()
			}

			/// Attempts to write the next item into the batch.
			pub fn try_write_next(&mut self, item: T) -> Result<(), T> {
				self.inner.try_write_next(item)
			}

			/// Writes the next item into the batch or panics if the batch is full.
			pub fn write_next(&mut self, item: T) {
				self.inner.write_next(item)
			}

			/// Writes items from an iterator until the batch is full.
			pub fn write_from_iter<I>(&mut self, iter: I) -> usize
			where
				I: IntoIterator<Item = T>,
			{
				self.inner.write_from_iter(iter)
			}

			/// Writes exactly `remaining()` items from an exact-size iterator.
			pub fn try_write_exact<I>(&mut self, iter: I) -> Result<(), $crate::permit::ExactLenMismatch>
			where
				I: IntoIterator<Item = T>,
				I::IntoIter: ExactSizeIterator,
			{
				self.inner.try_write_exact(iter)
			}

			/// Writes exactly `remaining()` items from an exact-size iterator, panicking on mismatch.
			pub fn write_exact<I>(&mut self, iter: I)
			where
				I: IntoIterator<Item = T>,
				I::IntoIter: ExactSizeIterator,
			{
				self.inner.write_exact(iter)
			}

			/// Fills remaining slots by repeatedly calling `make`.
			pub fn fill_with<G>(&mut self, make: G)
			where
				G: FnMut() -> T,
			{
				self.inner.fill_with(make)
			}

			/// Publishes the batch by filling remaining slots with the factory and committing.
			pub fn finish(self) {
				self.inner.finish()
			}
		}
	};

	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident < $lt:lifetime, T, F, const $n:ident : usize > = ( $publisher_sequencer:ty );
	) => {
		$(#[$meta])*
		$vis struct $name<$lt, T, F, const $n: usize>
		where
			F: Fn() -> T + Copy,
		{
			inner: $crate::permit::SendBatch<$lt, $publisher_sequencer, T, F>,
		}

		impl<T, F, const $n: usize> $name<'_, T, F, $n>
		where
			F: Fn() -> T + Copy,
		{
			/// Total number of items in the batch.
			pub fn capacity(&self) -> usize {
				self.inner.capacity()
			}

			/// Remaining items that must be written before the batch is full.
			pub fn remaining(&self) -> usize {
				self.inner.remaining()
			}

			/// Attempts to write the next item into the batch.
			pub fn try_write_next(&mut self, item: T) -> Result<(), T> {
				self.inner.try_write_next(item)
			}

			/// Writes the next item into the batch or panics if the batch is full.
			pub fn write_next(&mut self, item: T) {
				self.inner.write_next(item)
			}

			/// Writes items from an iterator until the batch is full.
			pub fn write_from_iter<I>(&mut self, iter: I) -> usize
			where
				I: IntoIterator<Item = T>,
			{
				self.inner.write_from_iter(iter)
			}

			/// Writes exactly `remaining()` items from an exact-size iterator.
			pub fn try_write_exact<I>(&mut self, iter: I) -> Result<(), $crate::permit::ExactLenMismatch>
			where
				I: IntoIterator<Item = T>,
				I::IntoIter: ExactSizeIterator,
			{
				self.inner.try_write_exact(iter)
			}

			/// Writes exactly `remaining()` items from an exact-size iterator, panicking on mismatch.
			pub fn write_exact<I>(&mut self, iter: I)
			where
				I: IntoIterator<Item = T>,
				I::IntoIter: ExactSizeIterator,
			{
				self.inner.write_exact(iter)
			}

			/// Fills remaining slots by repeatedly calling `make`.
			pub fn fill_with<G>(&mut self, make: G)
			where
				G: FnMut() -> T,
			{
				self.inner.fill_with(make)
			}

			/// Publishes the batch by filling remaining slots with the factory and committing.
			pub fn finish(self) {
				self.inner.finish()
			}
		}
	};
}

#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_recv_guard {
	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident < $lt:lifetime, T > = ( $consumer_sequencer:ty );
	) => {
		$(#[$meta])*
		$vis struct $name<$lt, T> {
			inner: $crate::consumers::ReadGuard<$lt, $consumer_sequencer, T>,
		}

		impl<T> ::std::ops::Deref for $name<'_, T> {
			type Target = T;

			fn deref(&self) -> &Self::Target {
				::std::ops::Deref::deref(&self.inner)
			}
		}
	};
}

#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_recv_iter {
	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident < $lt:lifetime, T > = ( $consumer_sequencer:ty );
	) => {
		$(#[$meta])*
		$vis struct $name<$lt, T> {
			inner: $crate::consumers::ReadIter<$lt, $consumer_sequencer, T>,
		}

		impl<$lt, T: $lt> ::std::iter::Iterator for $name<$lt, T> {
			type Item = &$lt T;

			fn next(&mut self) -> Option<Self::Item> {
				self.inner.next()
			}
		}
	};
}

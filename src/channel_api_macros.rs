/// Internal macros for keeping the public channel API consistent across
/// multiple topologies without duplicating wrapper code.
///
/// These macros are *crate implementation details*.
#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_sender {
	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident<T> {
			inner: $publisher_ty:ty,
		}
		=> SendBatch = $send_batch:ident;
	) => {
		$(#[$meta])*
		$vis struct $name<T> {
			inner: $publisher_ty,
		}

		impl<T> $name<T> {
			/// Attempts to send a single item.
			pub fn try_send(&mut self, item: T) -> Result<(), $crate::errors::TrySendError> {
				self.inner.try_publish(item)
			}

			/// Attempts to claim a contiguous range of `n` slots and returns a guard that
			/// writes/commits the range.
			///
			/// The returned batch commits automatically on drop.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `factory`, and those factory-produced values are
			///   published as if the user wrote them.
			/// - `factory` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `n == 0`.
			pub fn try_send_many<F>(
				&mut self,
				n: usize,
				factory: F,
			) -> Result<$send_batch<'_, T, F>, $crate::errors::TrySendError>
			where
				F: Fn() -> T + Copy,
			{
				let inner = self.inner.try_publish_many(n, factory)?;
				Ok($send_batch { inner })
			}

			/// Attempts to claim a contiguous range of `n` slots using `T::default()` as the fill factory.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `T::default()`, and those values are published.
			/// - `T::default()` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `n == 0`.
			pub fn try_send_many_default(
				&mut self,
				n: usize,
			) -> Result<$send_batch<'_, T, fn() -> T>, $crate::errors::TrySendError>
			where
				T: Default,
			{
				let inner = self.inner.try_publish_many_default(n)?;
				Ok($send_batch { inner })
			}

			/// Attempts to send up to `limit` items, claiming as many slots as available.
			///
			/// Returns a batch with the actually claimed slots (1..=limit).
			/// Returns `Full` if zero slots are available.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `factory`, and those factory-produced values are
			///   published as if the user wrote them.
			/// - `factory` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `limit == 0`.
			pub fn try_send_at_most<F>(
				&mut self,
				limit: usize,
				factory: F,
			) -> Result<$send_batch<'_, T, F>, $crate::errors::TrySendAtMostError>
			where
				F: Fn() -> T + Copy,
			{
				let inner = self.inner.try_publish_at_most(limit, factory)?;
				Ok($send_batch { inner })
			}

			/// Attempts to send up to `limit` items using `T::default()` as the fill factory.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `T::default()`, and those values are published.
			/// - `T::default()` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `limit == 0`.
			pub fn try_send_at_most_default(
				&mut self,
				limit: usize,
			) -> Result<$send_batch<'_, T, fn() -> T>, $crate::errors::TrySendAtMostError>
			where
				T: Default,
			{
				let inner = self.inner.try_publish_at_most_default(limit)?;
				Ok($send_batch { inner })
			}
		}
	};

	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident<T, const $n:ident: usize> {
			inner: $publisher_ty:ty,
		}
		=> SendBatch = $send_batch:ident;
	) => {
		$(#[$meta])*
		$vis struct $name<T, const $n: usize> {
			inner: $publisher_ty,
		}

		impl<T, const $n: usize> $name<T, $n> {
			/// Attempts to send a single item.
			pub fn try_send(&mut self, item: T) -> Result<(), $crate::errors::TrySendError> {
				self.inner.try_publish(item)
			}

			/// Attempts to claim a contiguous range of `n` slots and returns a guard that
			/// writes/commits the range.
			///
			/// The returned batch commits automatically on drop.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `factory`, and those factory-produced values are
			///   published as if the user wrote them.
			/// - `factory` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `n == 0`.
			pub fn try_send_many<F>(
				&mut self,
				n: usize,
				factory: F,
			) -> Result<$send_batch<'_, T, F, $n>, $crate::errors::TrySendError>
			where
				F: Fn() -> T + Copy,
			{
				let inner = self.inner.try_publish_many(n, factory)?;
				Ok($send_batch { inner })
			}

			/// Attempts to claim a contiguous range of `n` slots using `T::default()` as the fill factory.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `T::default()`, and those values are published.
			/// - `T::default()` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `n == 0`.
			pub fn try_send_many_default(
				&mut self,
				n: usize,
			) -> Result<$send_batch<'_, T, fn() -> T, $n>, $crate::errors::TrySendError>
			where
				T: Default,
			{
				let inner = self.inner.try_publish_many_default(n)?;
				Ok($send_batch { inner })
			}

			/// Attempts to send up to `limit` items, claiming as many slots as available.
			///
			/// Returns a batch with the actually claimed slots (1..=limit).
			/// Returns `Full` if zero slots are available.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `factory`, and those factory-produced values are
			///   published as if the user wrote them.
			/// - `factory` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `limit == 0`.
			pub fn try_send_at_most<F>(
				&mut self,
				limit: usize,
				factory: F,
			) -> Result<$send_batch<'_, T, F, $n>, $crate::errors::TrySendAtMostError>
			where
				F: Fn() -> T + Copy,
			{
				let inner = self.inner.try_publish_at_most(limit, factory)?;
				Ok($send_batch { inner })
			}

			/// Attempts to send up to `limit` items using `T::default()` as the fill factory.
			///
			/// # Contract
			///
			/// - If the batch is dropped before all items are written, the remaining slots are
			///   filled by repeatedly calling `T::default()`, and those values are published.
			/// - `T::default()` must not panic. If it panics (including during drop), the claimed
			///   range may never be committed/published, which can permanently reduce capacity
			///   or wedge progress.
			///
			/// # Panics
			///
			/// Panics if `limit == 0`.
			pub fn try_send_at_most_default(
				&mut self,
				limit: usize,
			) -> Result<$send_batch<'_, T, fn() -> T, $n>, $crate::errors::TrySendAtMostError>
			where
				T: Default,
			{
				let inner = self.inner.try_publish_at_most_default(limit)?;
				Ok($send_batch { inner })
			}
		}
	};
}

#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_receiver {
	(
		$(#[$meta:meta])*
		$vis:vis struct $name:ident<T> {
			inner: $consumer_ty:ty,
		}
		=> RecvGuard = $recv_guard:ident, RecvIter = $recv_iter:ident;
	) => {
		$(#[$meta])*
		$vis struct $name<T> {
			inner: $consumer_ty,
		}

		impl<T> $name<T> {
			/// Attempts to receive a single item.
			///
			/// The returned guard commits consumption on drop.
			///
			/// # Important
			///
			/// Dropping the guard marks the item as consumed.
			pub fn try_recv(&mut self) -> Result<$recv_guard<'_, T>, $crate::errors::TryRecvError> {
				let inner = self.inner.try_recv()?;
				Ok($recv_guard { inner })
			}

			/// Attempts to receive up to `n` items.
			///
			/// The returned batch commits the consumed range on drop.
			///
			/// # Important
			///
			/// Dropping the batch commits the *entire* range, even if you did not iterate it.
			/// Any unread items in the batch are skipped (considered consumed).
			///
			/// # Panics
			///
			/// Panics if `n == 0`.
			pub fn try_recv_many(&mut self, n: usize) -> Result<$recv_iter<'_, T>, $crate::errors::TryRecvError> {
				let inner = self.inner.try_recv_many(n as i64)?;
				Ok($recv_iter { inner })
			}

			/// Attempts to receive up to `limit` items, consuming as many as available.
			///
			/// Returns a batch with the actually consumed items (1..=limit).
			/// Returns `Empty` if zero items are available.
			///
			/// # Panics
			///
			/// Panics if `limit == 0`.
			pub fn try_recv_at_most(
				&mut self,
				limit: usize,
			) -> Result<$recv_iter<'_, T>, $crate::errors::TryRecvAtMostError> {
				let inner = self.inner.try_recv_at_most(limit as i64)?;
				Ok($recv_iter { inner })
			}
		}
	};
}

#[doc(hidden)]
#[macro_export]
macro_rules! channel_define_send_batch {
	(
		$vis:vis struct $name:ident < $lt:lifetime, T, F > = ( $publisher_sequencer:ty );
	) => {
		/// A guard representing an already-claimed contiguous range of slots.
		///
		/// Dropping this guard commits the whole range.
		///
		/// # Important
		///
		/// If the batch is dropped before all items are written, the remaining slots are
		/// filled by repeatedly calling the `factory` provided to `try_send_many*` /
		/// `try_send_at_most*` and those factory-produced values are published.
		///
		/// `factory` must not panic. A panic while filling (including during drop) can
		/// leave the claimed range uncommitted and may permanently reduce capacity or
		/// wedge progress.
		#[must_use = "SendBatch is a publish-on-drop guard; dropping it publishes any unwritten slots filled via the factory. Write the full batch and call finish() when ready."]
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
		$vis:vis struct $name:ident < $lt:lifetime, T, F, const $n:ident : usize > = ( $publisher_sequencer:ty );
	) => {
		/// A guard representing an already-claimed contiguous range of slots.
		///
		/// Dropping this guard commits the whole range.
		///
		/// # Important
		///
		/// If the batch is dropped before all items are written, the remaining slots are
		/// filled by repeatedly calling the `factory` provided to `try_send_many*` /
		/// `try_send_at_most*` and those factory-produced values are published.
		///
		/// `factory` must not panic. A panic while filling (including during drop) can
		/// leave the claimed range uncommitted and may permanently reduce capacity or
		/// wedge progress.
		#[must_use = "SendBatch is a publish-on-drop guard; dropping it publishes any unwritten slots filled via the factory. Write the full batch and call finish() when ready."]
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
		$vis:vis struct $name:ident < $lt:lifetime, T > = ( $consumer_sequencer:ty );
	) => {
		/// RAII guard for a received item.
		///
		/// Dereferences to `&T`.
		///
		/// Dropping the guard commits consumption of the item.
		///
		/// Holding the guard delays committing consumption, which can in turn delay slot reuse
		/// and apply backpressure to the channel.
		#[must_use = "RecvGuard commits consumption on drop; keep it alive while using the referenced item."]
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
		$vis:vis struct $name:ident < $lt:lifetime, T > = ( $consumer_sequencer:ty );
	) => {
		/// A batch view over a received range.
		///
		/// This guard commits the full claimed range on drop.
		///
		/// # Important
		///
		/// Dropping this guard commits the *entire* claimed range, even if you never called
		/// [`Self::iter`]. Any unread items in the batch are skipped (considered consumed).
		///
		/// Use [`Self::iter`] to iterate over `&T` safely.
		#[must_use = "RecvIter commits the whole batch on drop; iterate it before dropping if you need the items."]
		$vis struct $name<$lt, T> {
			inner: $crate::consumers::ReadBatch<$lt, $consumer_sequencer, T>,
		}

		impl<$lt, T: $lt> $name<$lt, T> {
			/// Iterates the items in this batch.
			///
			/// The returned iterator yields `&T` values whose lifetime is tied to the borrow
			/// of this batch guard, preventing them from outliving the commit-on-drop boundary.
			pub fn iter(&self) -> impl ::std::iter::Iterator<Item = &T> + '_ {
				self.inner.iter()
			}

			/// Commits the claimed range immediately.
			///
			/// This is equivalent to dropping the guard.
			pub fn finish(self) {
				self.inner.finish();
			}
		}
	};
}

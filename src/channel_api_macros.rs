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
			) -> Result<$recv_iter<'_, T>, $crate::errors::TryRecvError> {
				let inner = self.inner.try_recv_at_most(limit as i64)?;
				Ok($recv_iter { inner })
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

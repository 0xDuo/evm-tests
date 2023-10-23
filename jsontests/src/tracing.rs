use serde::Deserialize;
use std::{cell::RefCell, ptr::NonNull, rc::Rc};

/// Capture all events from `SputnikVM` emitted from within the given closure using the given listener.
pub fn traced_call<T, R, F>(listener: &mut T, f: F) -> R
where
	T: evm::gasometer::tracing::EventListener
		+ evm_runtime::tracing::EventListener
		+ evm::tracing::EventListener
		+ 'static,
	F: FnOnce() -> R,
{
	let mut gas_listener = SharedMutableReference::new(listener);
	let mut runtime_listener = gas_listener.clone();
	let mut evm_listener = gas_listener.clone();

	evm::gasometer::tracing::using(&mut gas_listener, || {
		evm_runtime::tracing::using(&mut runtime_listener, || {
			evm::tracing::using(&mut evm_listener, f)
		})
	})
}

/// This is a newtype over `u64` which allows the value to be missing.
/// The reason for this type is because comparing devm and Sputnik
/// gas values is not easy in some error cases. In such cases it does not
/// really matter what gas values are present on the step where the error
/// occurred because it is not observable to a user (the observable is how
/// much gas remains on the next step). With this type we can avoid checking
/// for gas equality by setting the value as `None`.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(from = "u64")]
pub struct TracingGas {
	pub value: Option<u64>,
}

impl From<u64> for TracingGas {
	fn from(value: u64) -> Self {
		Self { value: Some(value) }
	}
}

impl PartialEq for TracingGas {
	fn eq(&self, other: &Self) -> bool {
		match (self.value, other.value) {
			(None, None) | (None, Some(_)) | (Some(_), None) => true,
			(Some(x), Some(y)) => x == y,
		}
	}
}

impl PartialOrd for TracingGas {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		self.value
			.as_ref()
			.and_then(|x| other.value.as_ref().and_then(|y| x.partial_cmp(y)))
	}
}

impl Default for TracingGas {
	fn default() -> Self {
		Self { value: Some(0) }
	}
}

impl std::ops::SubAssign for TracingGas {
	fn sub_assign(&mut self, rhs: Self) {
		self.value.as_mut().and_then(|x| rhs.value.map(|y| *x -= y));
	}
}

/// This structure is intentionally private to this module as it is memory unsafe (contains a raw pointer).
/// Its purpose here is to allow a single event handling object to be used as the listener for
/// all `SputnikVM` events. It is needed because the listener must be passed as an object with a `'static`
/// lifetime, hence a normal reference cannot be used and we resort to raw pointers. The usage of this
/// struct in this module is safe because the `SharedMutableReference` objects created do not outlive
/// the reference they are based on (see `pub fn traced_call`). Moreover, because the `SputnikVM` code
/// is single-threaded, we do not need to worry about race conditions.
struct SharedMutableReference<T> {
	pointer: Rc<RefCell<NonNull<T>>>,
}

impl<T> SharedMutableReference<T> {
	fn new(reference: &mut T) -> Self {
		let ptr = NonNull::new(reference).unwrap();
		Self {
			pointer: Rc::new(RefCell::new(ptr)),
		}
	}

	fn clone(&self) -> Self {
		Self {
			pointer: Rc::clone(&self.pointer),
		}
	}
}

impl<T: evm::gasometer::tracing::EventListener> evm::gasometer::tracing::EventListener
	for SharedMutableReference<T>
{
	fn event(&mut self, event: evm::gasometer::tracing::Event) {
		unsafe {
			self.pointer.borrow_mut().as_mut().event(event);
		}
	}
}

impl<T: evm_runtime::tracing::EventListener> evm_runtime::tracing::EventListener
	for SharedMutableReference<T>
{
	fn event(&mut self, event: evm_runtime::tracing::Event) {
		unsafe {
			self.pointer.borrow_mut().as_mut().event(event);
		}
	}
}

impl<T: evm::tracing::EventListener> evm::tracing::EventListener for SharedMutableReference<T> {
	fn event(&mut self, event: evm::tracing::Event) {
		unsafe {
			self.pointer.borrow_mut().as_mut().event(event);
		}
	}
}

#[test]
fn test_gas_partial_eq() {
	let big: TracingGas = 10.into();
	let small: TracingGas = 2.into();
	let missing = TracingGas { value: None };

	// Two some values are equal iff the inner values are equal
	assert_eq!(big, big);
	assert_eq!(small, small);
	assert_ne!(big, small);

	// None values are equal to anything
	assert_eq!(big, missing);
	assert_eq!(small, missing);
	assert_eq!(missing, missing);
}

#[test]
fn test_gas_sub_assign() {
	let big: TracingGas = 10.into();
	let small: TracingGas = 2.into();
	let missing = TracingGas { value: None };

	// None minus anything is still None
	let mut x = missing;
	x -= big;
	assert_eq!(x.value, missing.value);
	x -= small;
	assert_eq!(x.value, missing.value);
	x -= missing;
	assert_eq!(x.value, missing.value);

	// Anything minus None does not change the value
	let mut x = big;
	x -= missing;
	assert_eq!(x.value, big.value);

	// Subtracting two present values does the subtraction
	let mut x = big;
	x -= small;
	assert_eq!(x.value, Some(big.value.unwrap() - small.value.unwrap()));
}

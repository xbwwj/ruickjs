use std::{marker::PhantomData, ptr::NonNull};

use rquickjs_sys::{
    JS_DupContext, JS_DupValueRT, JS_FreeContext, JS_FreeRuntime, JS_FreeValueRT, JS_NULL,
    JS_NewContext, JS_NewRuntime, JSContext, JSRuntime, JSValue,
};

pub struct Runtime {
    raw: NonNull<JSRuntime>,
}

impl Runtime {
    pub fn new() -> Option<Self> {
        let raw = unsafe { JS_NewRuntime() };

        NonNull::new(raw).map(|raw| Self { raw })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            JS_FreeRuntime(self.raw.as_ptr());
        }
    }
}

/// Defines how a handle is duplicated and released.
///
/// # Safety
///
/// Implementations must guarantee:
///
/// - `dup` creates an independently owned reference.
/// - `free` releases exactly one owned reference.
/// - `data` belongs to the provided Runtime.
/// - The returned pointer remains valid until `free`.
pub unsafe trait GcHeader: Sized {
    unsafe fn dup(runtime: NonNull<JSRuntime>, data: NonNull<Self>) -> NonNull<Self>;
    unsafe fn free(runtime: NonNull<JSRuntime>, data: NonNull<Self>);
}

pub trait Handle {
    type Data: GcHeader;

    fn runtime(&self) -> NonNull<JSRuntime>;

    unsafe fn get_unchecked(&self) -> &Self::Data;
}

impl<H: Handle + ?Sized> Handle for &H {
    type Data = H::Data;

    fn runtime(&self) -> NonNull<JSRuntime> {
        (**self).runtime()
    }

    unsafe fn get_unchecked(&self) -> &Self::Data {
        unsafe { (**self).get_unchecked() }
    }
}

pub struct Local<'s, T: GcHeader> {
    runtime: NonNull<JSRuntime>,
    data: NonNull<T>,
    _phantom: PhantomData<&'s ()>,
}

impl<'s, T: GcHeader> Local<'s, T> {
    fn from_raw(runtime: &'s Runtime, data: NonNull<T>) -> Self {
        Self {
            runtime: runtime.raw,
            data,
            _phantom: PhantomData,
        }
    }

    pub fn as_ref(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

impl<T: GcHeader> Clone for Local<'_, T> {
    fn clone(&self) -> Self {
        let data = unsafe { T::dup(self.runtime, self.data) };

        Self {
            runtime: self.runtime,
            data,
            _phantom: PhantomData,
        }
    }
}

impl<T: GcHeader> Drop for Local<'_, T> {
    fn drop(&mut self) {
        unsafe {
            T::free(self.runtime, self.data);
        }
    }
}

impl<T: GcHeader> Handle for Local<'_, T> {
    type Data = T;

    fn runtime(&self) -> NonNull<JSRuntime> {
        self.runtime
    }

    unsafe fn get_unchecked(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

pub struct Global<T: GcHeader> {
    runtime: NonNull<JSRuntime>,
    data: NonNull<T>,
}

impl<T: GcHeader> Global<T> {
    /// Creates a Global by duplicating an existing handle.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the Runtime outlives
    /// this Global and all its clones.
    pub unsafe fn new(runtime: &Runtime, handle: impl Handle<Data = T>) -> Self {
        assert_eq!(
            runtime.raw,
            handle.runtime(),
            "handle belongs to a different Runtime",
        );

        let source = NonNull::from(unsafe { handle.get_unchecked() });

        let data = unsafe { T::dup(runtime.raw, source) };

        Self {
            runtime: runtime.raw,
            data,
        }
    }

    /// Takes ownership of an existing raw handle.
    ///
    /// # Safety
    ///
    /// - `data` must represent exactly one owned reference.
    /// - `data` must belong to `runtime`.
    /// - `runtime` must outlive this Global and all its clones.
    pub unsafe fn from_raw(runtime: &Runtime, data: NonNull<T>) -> Self {
        Self {
            runtime: runtime.raw,
            data,
        }
    }

    /// Opens this Global as a borrowed reference.
    ///
    /// # Safety
    ///
    /// `runtime` must be the Runtime owning this Global,
    /// and must remain alive for the returned reference.
    pub unsafe fn open<'a>(&'a self, runtime: &'a Runtime) -> &'a T {
        assert_eq!(
            self.runtime, runtime.raw,
            "Global belongs to a different Runtime",
        );

        unsafe { self.data.as_ref() }
    }
}

impl<T: GcHeader> Clone for Global<T> {
    fn clone(&self) -> Self {
        let data = unsafe { T::dup(self.runtime, self.data) };

        Self {
            runtime: self.runtime,
            data,
        }
    }
}

impl<T: GcHeader> Drop for Global<T> {
    fn drop(&mut self) {
        unsafe {
            T::free(self.runtime, self.data);
        }
    }
}

impl<T: GcHeader> Handle for Global<T> {
    type Data = T;

    fn runtime(&self) -> NonNull<JSRuntime> {
        self.runtime
    }

    unsafe fn get_unchecked(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

#[repr(transparent)]
pub struct Context(JSContext);

impl Context {
    pub fn new<'s>(runtime: &'s Runtime) -> Option<Local<'s, Context>> {
        let raw = unsafe { JS_NewContext(runtime.raw.as_ptr()) };

        let data = NonNull::new(raw.cast::<Context>())?;

        Some(Local::from_raw(runtime, data))
    }
}

unsafe impl GcHeader for Context {
    unsafe fn dup(_runtime: NonNull<JSRuntime>, data: NonNull<Self>) -> NonNull<Self> {
        let raw = unsafe { JS_DupContext(data.cast::<JSContext>().as_ptr()) };

        // JS_DupContext returns the same context pointer
        // with an incremented reference count.
        unsafe { NonNull::new_unchecked(raw.cast::<Context>()) }
    }

    unsafe fn free(_runtime: NonNull<JSRuntime>, data: NonNull<Self>) {
        unsafe {
            JS_FreeContext(data.cast::<JSContext>().as_ptr());
        }
    }
}

// FIXME: it should not be value, but object instead.
// But qjs resemble more like mozjs rather than v8 handle.
#[repr(transparent)]
pub struct Value(JSValue);

impl Value {
    pub fn new_null<'s>(runtime: &'s Runtime) -> Local<'s, Value> {
        let data = NonNull::from(Box::leak(Box::new(Value(JS_NULL))));
        Local::from_raw(runtime, data)
    }
}

unsafe impl GcHeader for Value {
    unsafe fn dup(runtime: NonNull<JSRuntime>, data: NonNull<Self>) -> NonNull<Self> {
        let value = unsafe { data.as_ref().0 };
        let duplicated = unsafe { JS_DupValueRT(runtime.as_ptr(), value) };
        NonNull::from(Box::leak(Box::new(Value(duplicated))))
    }

    unsafe fn free(runtime: NonNull<JSRuntime>, data: NonNull<Self>) {
        let value = unsafe { Box::from_raw(data.as_ptr()) };
        unsafe {
            JS_FreeValueRT(runtime.as_ptr(), value.0);
        }
    }
}

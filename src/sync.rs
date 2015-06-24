use context::CommandContext;
use version::Api;
use version::Version;
use gl;

use backend::Facade;
use context::Context;
use ContextExt;
use std::rc::Rc;

use std::thread;

/// Provides a way to wait for a server-side operation to be finished.
///
/// Creating a `SyncFence` injects an element in the commands queue of the backend.
/// When this element is reached, the fence becomes signaled.
///
/// ## Example
///
/// ```no_run
/// # let display: glium::Display = unsafe { std::mem::uninitialized() };
/// # fn do_something<T>(_: &T) {}
/// let fence = glium::SyncFence::new_if_supported(&display).unwrap();
/// do_something(&display);
/// fence.wait();   // blocks until the previous operations have finished
/// ```
pub struct SyncFence {
    context: Rc<Context>,
    id: Option<gl::types::GLsync>,
}

impl SyncFence {
    /// Builds a new `SyncFence` that is injected in the server.
    ///
    /// # Features
    ///
    /// Only available if the `gl_sync` feature is enabled.
    #[cfg(feature = "gl_sync")]
    pub fn new<F>(facade: &F) -> SyncFence where F: Facade {
        SyncFence::new_if_supported(facade).unwrap()
    }

    /// Builds a new `SyncFence` that is injected in the server.
    ///
    /// Returns `None` is this is not supported by the backend.
    pub fn new_if_supported<F>(facade: &F) -> Option<SyncFence> where F: Facade {
        let mut ctxt = facade.get_context().make_current();

        unsafe { new_linear_sync_fence_if_supported(&mut ctxt) }
            .map(|f| f.into_sync_fence(facade))
    }

    /// Blocks until the operation has finished on the server.
    pub fn wait(mut self) {
        let sync = self.id.take().unwrap();

        let mut ctxt = self.context.make_current();
        let result = unsafe { client_wait(&mut ctxt, sync) };
        unsafe { delete_fence(&mut ctxt, sync) };

        match result {
            gl::ALREADY_SIGNALED | gl::CONDITION_SATISFIED => (),
            _ => panic!("Could not wait for the fence")
        };
    }
}

impl Drop for SyncFence {
    fn drop(&mut self) {
        let sync = match self.id {
            None => return,     // fence has already been deleted
            Some(s) => s
        };

        let mut ctxt = self.context.make_current();
        unsafe { delete_fence(&mut ctxt, sync) };
    }
}

/// Prototype for a `SyncFence`.
///
/// The fence must be consumed with either `into_sync_fence`, otherwise
/// the destructor will panic.
#[must_use]
pub struct LinearSyncFence {
    id: Option<gl::types::GLsync>,
}

unsafe impl Send for LinearSyncFence {}

impl LinearSyncFence {
    /// Turns the prototype into a real fence.
    pub fn into_sync_fence<F>(mut self, facade: &F) -> SyncFence where F: Facade {
        SyncFence {
            context: facade.get_context().clone(),
            id: self.id.take()
        }
    }
}

impl Drop for LinearSyncFence {
    fn drop(&mut self) {
        if !thread::panicking() {
            assert!(self.id.is_none());
        }
    }
}

#[cfg(feature = "gl_sync")]
pub unsafe fn new_linear_sync_fence(ctxt: &mut CommandContext) -> LinearSyncFence {
    LinearSyncFence {
        id: Some(ctxt.gl.FenceSync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0)),
    }
}

pub unsafe fn new_linear_sync_fence_if_supported(ctxt: &mut CommandContext)
                                                 -> Option<LinearSyncFence>
{
    if ctxt.version >= &Version(Api::Gl, 3, 2) ||
       ctxt.version >= &Version(Api::GlEs, 3, 0) || ctxt.extensions.gl_arb_sync
    {
        Some(LinearSyncFence {
            id: Some(ctxt.gl.FenceSync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0)),
        })

    } else if ctxt.extensions.gl_apple_sync {
        Some(LinearSyncFence {
            id: Some(ctxt.gl.FenceSyncAPPLE(gl::SYNC_GPU_COMMANDS_COMPLETE_APPLE, 0)),
        })

    } else {
        None
    }
}

/// Waits for this fence and destroys it, from within the commands context.
pub unsafe fn wait_linear_sync_fence_and_drop(mut fence: LinearSyncFence,
                                              ctxt: &mut CommandContext)
{
    let fence = fence.id.take().unwrap();
    client_wait(ctxt, fence);
    delete_fence(ctxt, fence);
}

/// Destroys a fence, from within the commands context.
pub unsafe fn destroy_linear_sync_fence(ctxt: &mut CommandContext, mut fence: LinearSyncFence) {
    let fence = fence.id.take().unwrap();
    delete_fence(ctxt, fence);
}

/// Calls `glClientWaitSync` and returns the result.
///
/// Tries without flushing first, then with flushing.
///
/// # Unsafety
///
/// The fence object must exist.
///
unsafe fn client_wait(ctxt: &mut CommandContext, fence: gl::types::GLsync) -> gl::types::GLenum {
    // trying without flushing first
    let result = if ctxt.version >= &Version(Api::Gl, 3, 2) ||
                    ctxt.version >= &Version(Api::GlEs, 3, 0) || ctxt.extensions.gl_arb_sync
    {
        ctxt.gl.ClientWaitSync(fence, 0, 0)
    } else if ctxt.extensions.gl_apple_sync {
        ctxt.gl.ClientWaitSyncAPPLE(fence, 0, 0)
    } else {
        unreachable!();
    };

    match result {
        val @ gl::ALREADY_SIGNALED | val @ gl::CONDITION_SATISFIED => return val,
        gl::TIMEOUT_EXPIRED => (),
        gl::WAIT_FAILED => (),
        _ => unreachable!()
    };

    // waiting with a deadline of one year
    // the reason why the deadline is so long is because if you attach a GL debugger,
    // the wait can be blocked during a breaking point of the debugger
    if ctxt.version >= &Version(Api::Gl, 3, 2) ||
       ctxt.version >= &Version(Api::GlEs, 3, 0) || ctxt.extensions.gl_arb_sync
    {
        ctxt.gl.ClientWaitSync(fence, gl::SYNC_FLUSH_COMMANDS_BIT,
                               365 * 24 * 3600 * 1000 * 1000 * 1000)
    } else if ctxt.extensions.gl_apple_sync {
        ctxt.gl.ClientWaitSyncAPPLE(fence, gl::SYNC_FLUSH_COMMANDS_BIT_APPLE,
                                    365 * 24 * 3600 * 1000 * 1000 * 1000)
    } else {
        unreachable!();
    }
}

/// Deletes a fence.
///
/// # Unsafety
///
/// The fence object must exist.
///
unsafe fn delete_fence(ctxt: &mut CommandContext, fence: gl::types::GLsync) {
    if ctxt.version >= &Version(Api::Gl, 3, 2) ||
       ctxt.version >= &Version(Api::GlEs, 3, 0) || ctxt.extensions.gl_arb_sync
    {
        ctxt.gl.DeleteSync(fence);
    } else if ctxt.extensions.gl_apple_sync {
        ctxt.gl.DeleteSyncAPPLE(fence);
    } else {
        unreachable!();
    };
}

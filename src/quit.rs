use gpui_kit::App;

pub(crate) fn approved(cx: &App) {
    #[cfg(target_os = "macos")]
    native::ALLOW_QUIT.set(true);
    cx.quit();
}

#[cfg(target_os = "macos")]
pub(crate) mod native {
    use gpui_kit::{App, AsyncApp};
    use objc2::{
        Encode,
        runtime::{AnyClass, AnyObject, Imp, Sel},
        sel,
    };
    use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};
    use std::{
        cell::{Cell, RefCell},
        ffi::CString,
    };

    thread_local! {
        pub(super) static ALLOW_QUIT: Cell<bool> = const { Cell::new(false) };
        static APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
    }

    pub(crate) fn init(cx: &App) {
        APP.set(Some(cx.to_async()));
    }

    pub(crate) fn install() {
        let class = AnyClass::get(c"GPUIApplicationDelegate")
            .expect("GPUI macOS application delegate is unavailable");
        let selector = sel!(applicationShouldTerminate:);
        assert!(
            class.instance_method(selector).is_none(),
            "GPUI now implements its own quit guard; review the application quit integration"
        );
        let encoding = CString::new(format!("{}@:@", NSApplicationTerminateReply::ENCODING))
            .expect("Invalid application delegate method encoding");
        // Add the missing optional delegate method before GPUI assigns its delegate.
        // The exact callback signature and generated encoding match AppKit's contract.
        let installed = unsafe {
            let implementation = std::mem::transmute::<
                unsafe extern "C-unwind" fn(
                    &AnyObject,
                    Sel,
                    &NSApplication,
                ) -> NSApplicationTerminateReply,
                Imp,
            >(should_terminate);
            objc2::ffi::class_addMethod(
                (class as *const AnyClass).cast_mut(),
                selector,
                implementation,
                encoding.as_ptr(),
            )
        };
        assert!(
            installed.as_bool(),
            "Could not install the macOS quit guard"
        );
    }

    unsafe extern "C-unwind" fn should_terminate(
        _: &AnyObject,
        _: Sel,
        _: &NSApplication,
    ) -> NSApplicationTerminateReply {
        if ALLOW_QUIT.replace(false) {
            return NSApplicationTerminateReply::TerminateNow;
        }
        if let Some(cx) = APP.with_borrow(Clone::clone) {
            // AppKit can call this while GPUI is borrowed. Defer to the main queue.
            cx.spawn(async |cx| cx.update(crate::request_quit)).detach();
        }
        NSApplicationTerminateReply::TerminateCancel
    }
}

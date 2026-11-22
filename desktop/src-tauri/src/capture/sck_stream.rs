//! The ScreenCaptureKit stream itself: one `SCStream`, its delegate and its
//! sample-buffer output, on one serial dispatch queue.
//!
//! This is `scap_screencapturekit::Capturer` (Cap, MIT, rev 52b0acc) brought
//! into the tree with one difference: every Screen sample buffer reaches the
//! callback, including the ones that carry no image. SCK sends those as
//! status-only buffers (idle, blank, suspended, stopped), and the scap wrapper
//! dropped them before our code could see them. On macOS 12.7.6 every take
//! recorded no video at all, and whether SCK was sending nothing
//! or sending only status buffers is exactly the question the diagnostics
//! need to answer, so the callback has to see them. What happens to a buffer
//! with an image is unchanged: `capture/macos.rs` forwards it as before.
//!
//! Stream setup order matches scap's: audio output (macOS 13+ and only when
//! asked for) first, then the screen output, both on the same queue.

#![cfg(target_os = "macos")]

use cidre::{
    arc, cm, define_obj_type, dispatch, ns, objc,
    sc::{self, StreamDelegate, StreamDelegateImpl, StreamOutput, StreamOutputImpl},
};

/// A sample buffer as SCK delivered it, before anything is filtered.
pub enum StreamSample<'a> {
    /// A screen buffer. It may carry no image (a status-only buffer).
    Screen(&'a cm::SampleBuf),
    Audio(&'a cm::SampleBuf),
}

type SampleCallback = Box<dyn FnMut(StreamSample<'_>)>;
type StopCallback = Box<dyn FnMut(&ns::Error)>;

#[derive(Default)]
pub struct StreamCallbacksInner {
    sample_cb: Option<SampleCallback>,
    stop_cb: Option<StopCallback>,
}

// The class name must differ from scap's `CAPTURER`: that crate is still
// linked (permission checks, config builder) and registers its own class.
define_obj_type!(
    pub StreamCallbacks + StreamOutputImpl + StreamDelegateImpl,
    StreamCallbacksInner,
    CLIPY_SCK_STREAM_CALLBACKS
);

// SAFETY: same contract as scap's capturer callbacks. SCK calls both methods
// on the one serial queue the stream was given, never concurrently.
unsafe impl Send for StreamCallbacks {}
unsafe impl Sync for StreamCallbacks {}

impl sc::stream::Output for StreamCallbacks {}

#[objc::add_methods]
impl sc::stream::OutputImpl for StreamCallbacks {
    extern "C" fn impl_stream_did_output_sample_buf(
        &mut self,
        _cmd: Option<&objc::Sel>,
        _: &sc::Stream,
        sample_buf: &mut cm::SampleBuf,
        kind: sc::OutputType,
    ) {
        if let Some(cb) = &mut self.inner_mut().sample_cb {
            match kind {
                sc::OutputType::Screen => cb(StreamSample::Screen(sample_buf)),
                sc::OutputType::Audio => cb(StreamSample::Audio(sample_buf)),
                // Microphone output needs macOS 15 and is never added.
                sc::OutputType::Mic => {}
            }
        }
    }
}

impl sc::stream::Delegate for StreamCallbacks {}

#[objc::add_methods]
impl sc::stream::DelegateImpl for StreamCallbacks {
    extern "C" fn impl_stream_did_stop_with_err(
        &mut self,
        _cmd: Option<&objc::Sel>,
        _stream: &sc::Stream,
        error: &ns::Error,
    ) {
        if let Some(cb) = &mut self.inner_mut().stop_cb {
            cb(error)
        }
    }
}

pub struct SckStream {
    _queue: arc::R<dispatch::Queue>,
    stream: arc::R<sc::Stream>,
    // Must outlive the stream; SCK holds only a weak reference to it.
    _callbacks: arc::R<StreamCallbacks>,
}

impl SckStream {
    pub fn build(
        filter: &sc::ContentFilter,
        config: &sc::StreamCfg,
        add_audio_output: bool,
        sample_cb: impl FnMut(StreamSample<'_>) + 'static,
        stop_cb: impl FnMut(&ns::Error) + 'static,
    ) -> Result<Self, arc::R<ns::Error>> {
        let callbacks = StreamCallbacks::with(StreamCallbacksInner {
            sample_cb: Some(Box::new(sample_cb)),
            stop_cb: Some(Box::new(stop_cb)),
        });
        let stream = sc::Stream::with_delegate(filter, config, callbacks.as_ref());
        let queue = dispatch::Queue::serial_with_ar_pool();
        if add_audio_output {
            stream
                .add_stream_output(callbacks.as_ref(), sc::OutputType::Audio, Some(&queue))
                .map_err(|e| e.retained())?;
        }
        stream
            .add_stream_output(callbacks.as_ref(), sc::OutputType::Screen, Some(&queue))
            .map_err(|e| e.retained())?;
        Ok(Self {
            _queue: queue,
            stream,
            _callbacks: callbacks,
        })
    }

    pub async fn start(&self) -> Result<(), arc::R<ns::Error>> {
        self.stream.start().await
    }

    pub async fn stop(&self) -> Result<(), arc::R<ns::Error>> {
        self.stream.stop().await
    }
}

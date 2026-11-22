//! A minimal safe wrapper over sherpa-onnx's offline recognizer C API
//! (`sherpa-onnx-sys`): load a model, decode one piece of audio, read the
//! result JSON (text, tokens, token start times and, for Whisper, the
//! detected language).

/// What the recognizer said about one piece. Times are seconds from the
/// start of that piece.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PieceAnswer {
    pub text: String,
    pub tokens: Vec<String>,
    pub timestamps: Vec<f64>,
    pub lang: Option<String>,
}

impl PieceAnswer {
    /// Parse the recognizer's result JSON.
    #[cfg_attr(not(feature = "local"), allow(dead_code))]
    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let tokens: Vec<String> = v
            .get("tokens")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .map(|x| x.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        let timestamps: Vec<f64> = v
            .get("timestamps")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect())
            .unwrap_or_default();
        // Whisper reports "<|en|>" or "en"; keep the bare code.
        let lang = v
            .get("lang")
            .and_then(|l| l.as_str())
            .map(|l| {
                l.trim()
                    .trim_start_matches("<|")
                    .trim_end_matches("|>")
                    .to_string()
            })
            .filter(|l| !l.is_empty());
        Some(PieceAnswer {
            text,
            tokens,
            timestamps,
            lang,
        })
    }
}

#[cfg(feature = "local")]
pub(crate) use real::Recognizer;

#[cfg(feature = "local")]
mod real {
    use std::ffi::{CStr, CString};
    use std::path::Path;

    use sherpa_onnx_sys as sys;

    use super::PieceAnswer;
    use crate::audio::SAMPLE_RATE;
    use crate::models::{model_file_paths, LocalModelId};

    pub(crate) struct Recognizer {
        ptr: *const sys::OfflineRecognizer,
    }

    // SAFETY: the recognizer is created, used and destroyed on one worker
    // thread; it is only moved there, never shared.
    unsafe impl Send for Recognizer {}

    fn cstr(s: &str, keep: &mut Vec<CString>) -> Result<*const std::os::raw::c_char, String> {
        let c = CString::new(s).map_err(|_| "a model path contains a NUL byte".to_string())?;
        let p = c.as_ptr();
        keep.push(c);
        Ok(p)
    }

    impl Recognizer {
        pub fn new(model: LocalModelId, dir: &Path, threads: i32) -> Result<Self, String> {
            let paths = model_file_paths(model, dir);
            let path_of = |name: &str| -> Result<String, String> {
                let p = paths
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, p)| p)
                    .ok_or("missing model file")?;
                p.to_str()
                    .map(str::to_string)
                    .ok_or_else(|| "the model folder path is not valid UTF-8".to_string())
            };
            let mut keep = Vec::new();
            // SAFETY: every field of these C structs is a pointer or a number,
            // for which all-zero is the C API's "use the default".
            let mut cfg: sys::OfflineRecognizerConfig = unsafe { std::mem::zeroed() };
            cfg.feat_config = sys::FeatureConfig {
                sample_rate: SAMPLE_RATE as i32,
                feature_dim: 80,
            };
            cfg.model_config.tokens = cstr(&path_of("tokens.txt")?, &mut keep)?;
            cfg.model_config.num_threads = threads.max(1);
            cfg.model_config.debug = 0;
            cfg.model_config.provider = cstr("cpu", &mut keep)?;
            cfg.decoding_method = cstr("greedy_search", &mut keep)?;
            cfg.max_active_paths = 4;
            cfg.ctc_fst_decoder_config.max_active = 3000;
            match model {
                LocalModelId::EnglishFast => {
                    cfg.model_config.nemo_ctc.model =
                        cstr(&path_of("model.int8.onnx")?, &mut keep)?;
                }
                LocalModelId::Multilingual => {
                    cfg.model_config.whisper.encoder =
                        cstr(&path_of("encoder.int8.onnx")?, &mut keep)?;
                    cfg.model_config.whisper.decoder =
                        cstr(&path_of("decoder.int8.onnx")?, &mut keep)?;
                    cfg.model_config.whisper.language = cstr("", &mut keep)?; // detect
                    cfg.model_config.whisper.task = cstr("transcribe", &mut keep)?;
                    cfg.model_config.whisper.tail_paddings = -1;
                }
            }
            // SAFETY: cfg and the strings it points at (`keep`) outlive the call.
            let ptr = unsafe { sys::SherpaOnnxCreateOfflineRecognizer(&cfg) };
            drop(keep);
            if ptr.is_null() {
                return Err("the speech model could not be loaded".into());
            }
            Ok(Recognizer { ptr })
        }

        pub fn decode(&self, samples: &[f32]) -> Result<PieceAnswer, String> {
            let len = i32::try_from(samples.len()).map_err(|_| "piece too long".to_string())?;
            // SAFETY: plain C API calls on a live recognizer; the stream and
            // the result string are destroyed before returning.
            unsafe {
                let stream = sys::SherpaOnnxCreateOfflineStream(self.ptr);
                if stream.is_null() {
                    return Err("could not create a stream".into());
                }
                sys::SherpaOnnxAcceptWaveformOffline(
                    stream,
                    SAMPLE_RATE as i32,
                    samples.as_ptr(),
                    len,
                );
                sys::SherpaOnnxDecodeOfflineStream(self.ptr, stream);
                let json = sys::SherpaOnnxGetOfflineStreamResultAsJson(stream);
                let parsed = if json.is_null() {
                    None
                } else {
                    let s = CStr::from_ptr(json).to_string_lossy().into_owned();
                    sys::SherpaOnnxDestroyOfflineStreamResultJson(json);
                    PieceAnswer::from_json(&s)
                };
                sys::SherpaOnnxDestroyOfflineStream(stream);
                parsed.ok_or_else(|| "the recognizer returned no result".into())
            }
        }
    }

    impl Drop for Recognizer {
        fn drop(&mut self) {
            // SAFETY: created by SherpaOnnxCreateOfflineRecognizer, destroyed once.
            unsafe { sys::SherpaOnnxDestroyOfflineRecognizer(self.ptr) }
        }
    }
}

/// Stand-in when the crate is built without the `local` feature.
#[cfg(not(feature = "local"))]
pub(crate) struct Recognizer;

#[cfg(not(feature = "local"))]
impl Recognizer {
    pub fn new(
        _model: crate::models::LocalModelId,
        _dir: &std::path::Path,
        _threads: i32,
    ) -> Result<Self, String> {
        Err("this build has no local speech engine".into())
    }
    pub fn decode(&self, _samples: &[f32]) -> Result<PieceAnswer, String> {
        Err("this build has no local speech engine".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_parakeet_and_whisper_results() {
        let p = PieceAnswer::from_json(
            r#"{"text":" hi there","tokens":[" hi"," there"],"timestamps":[0.12,0.48],"lang":""}"#,
        )
        .unwrap();
        assert_eq!(p.tokens.len(), 2);
        assert_eq!(p.timestamps, vec![0.12, 0.48]);
        assert_eq!(p.lang, None);
        let w =
            PieceAnswer::from_json(r#"{"text":"Bonjour","tokens":["Bon","jour"],"lang":"<|fr|>"}"#)
                .unwrap();
        assert!(w.timestamps.is_empty());
        assert_eq!(w.lang.as_deref(), Some("fr"));
        assert!(PieceAnswer::from_json("not json").is_none());
    }
}

//! Pure planning and merging for chunked transcription: where to cut a long
//! recording (inside silences, so no word is split) and how to put the
//! pieces' segments back on one timeline. A line-for-line port of
//! `packages/transcribe/src/chunking.ts`; keep the two in step.
//!
//! The planner always returns pieces that fit the engine's hard limit,
//! cutting mid-audio when a stretch has no silence at all: the local edition
//! has no fallback model without a length limit.

use crate::Segment;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceSpan {
    pub start_sec: f64,
    pub end_sec: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkPlanOptions {
    /// Preferred length of a piece.
    pub target_sec: f64,
    /// Hard limit on the audio handed to the engine, overlap included.
    pub max_sec: f64,
    /// Padding added on both sides of every cut, so a word at a seam is whole in one piece.
    pub overlap_sec: f64,
    /// Never cut closer than this to the previous cut.
    pub min_sec: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkPlan {
    pub index: usize,
    /// Audio handed to the engine: [extract_start_sec, extract_start_sec + extract_dur_sec).
    pub extract_start_sec: f64,
    pub extract_dur_sec: f64,
    /// The core this piece owns; a token or segment is kept only when it falls inside.
    pub keep_start_sec: f64,
    pub keep_end_sec: f64,
    /// True when no silence was near and the piece was cut mid-audio.
    pub hard_cut: bool,
}

/// Plan pieces no longer than `max_sec`, cut at silence midpoints near
/// `target_sec`. A recording that fits in one piece is one piece.
pub fn plan_chunks(
    silences: &[SilenceSpan],
    total_sec: f64,
    opts: &ChunkPlanOptions,
) -> Vec<ChunkPlan> {
    let ChunkPlanOptions {
        target_sec,
        max_sec,
        overlap_sec,
        ..
    } = *opts;
    if total_sec.is_nan() || total_sec <= 0.0 {
        return Vec::new();
    }
    if total_sec <= max_sec {
        return vec![ChunkPlan {
            index: 0,
            extract_start_sec: 0.0,
            extract_dur_sec: total_sec,
            keep_start_sec: f64::NEG_INFINITY,
            keep_end_sec: f64::INFINITY,
            hard_cut: false,
        }];
    }
    // An inner piece carries overlap on both sides, the first and last on one.
    let inner_core = max_sec - 2.0 * overlap_sec;
    assert!(inner_core > 0.0, "chunk overlap leaves no room for audio");

    let mut mids: Vec<f64> = silences
        .iter()
        .filter(|s| s.end_sec > s.start_sec)
        .map(|s| (s.start_sec + s.end_sec) / 2.0)
        .filter(|m| *m > 0.0 && *m < total_sec)
        .collect();
    mids.sort_by(f64::total_cmp);

    let mut cuts: Vec<(f64, bool)> = Vec::new();
    let mut pos = 0.0f64;
    loop {
        let remaining = total_sec - pos;
        if pos > 0.0 && remaining + overlap_sec <= max_sec {
            break; // the rest is the last piece
        }
        let core_limit = if pos == 0.0 {
            max_sec - overlap_sec
        } else {
            inner_core
        };
        let min_sec = opts.min_sec.min(core_limit / 2.0);
        let lo = pos + min_sec;
        let hi = pos + core_limit;
        let target = pos + target_sec.min(core_limit);
        let mut best: Option<f64> = None;
        for &m in &mids {
            if m < lo || m > hi {
                continue;
            }
            if best.is_none_or(|b| (m - target).abs() < (b - target).abs()) {
                best = Some(m);
            }
        }
        let at = best.unwrap_or(target);
        cuts.push((at, best.is_none()));
        pos = at;
    }

    let mut bounds = vec![0.0];
    bounds.extend(cuts.iter().map(|c| c.0));
    bounds.push(total_sec);
    let last = bounds.len() - 2;
    (0..bounds.len() - 1)
        .map(|i| {
            let core_start = bounds[i];
            let core_end = bounds[i + 1];
            let extract_start_sec = (core_start - overlap_sec).max(0.0);
            let extract_end_sec = (core_end + overlap_sec).min(total_sec);
            ChunkPlan {
                index: i,
                extract_start_sec,
                extract_dur_sec: extract_end_sec - extract_start_sec,
                keep_start_sec: if i == 0 {
                    f64::NEG_INFINITY
                } else {
                    core_start
                },
                keep_end_sec: if i == last { f64::INFINITY } else { core_end },
                hard_cut: (i > 0 && cuts[i - 1].1) || (i < cuts.len() && cuts[i].1),
            }
        })
        .collect()
}

/// Seconds of [start_sec, end_sec) not covered by any silence.
pub fn speech_sec_in(silences: &[SilenceSpan], start_sec: f64, end_sec: f64) -> f64 {
    let mut silent = 0.0;
    for s in silences {
        let a = start_sec.max(s.start_sec);
        let b = end_sec.min(s.end_sec);
        if b > a {
            silent += b - a;
        }
    }
    (end_sec - start_sec - silent).max(0.0)
}

/// Shrink [start_sec, end_sec) past silences touching either edge.
pub fn trim_to_speech(silences: &[SilenceSpan], start_sec: f64, end_sec: f64) -> (f64, f64) {
    let mut a = start_sec;
    let mut b = end_sec;
    for s in silences {
        if s.start_sec <= a && s.end_sec > a {
            a = s.end_sec.min(b);
        }
        if s.start_sec < b && s.end_sec >= b {
            b = s.start_sec.max(a);
        }
    }
    (a, b)
}

/// A segment in seconds, relative to its piece's audio (or already on the
/// recording's timeline, for [`to_timeline`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RelativeSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct PieceResult {
    pub plan: ChunkPlan,
    pub segments: Vec<RelativeSegment>,
}

/// Put every piece's segments on the recording's timeline, keep each segment
/// only from the piece whose core holds its centre (so overlap is never
/// transcribed twice), then sort and nudge so segments never overlap.
pub fn merge_pieces(pieces: &[PieceResult]) -> Vec<Segment> {
    let mut kept: Vec<(f64, f64, String)> = Vec::new();
    for PieceResult { plan, segments } in pieces {
        for s in segments {
            let start = s.start + plan.extract_start_sec;
            let end = s.end.max(s.start) + plan.extract_start_sec;
            let centre = (start + end) / 2.0;
            if centre < plan.keep_start_sec || centre >= plan.keep_end_sec {
                continue;
            }
            let text = s.text.trim();
            if !text.is_empty() {
                kept.push((start, end, text.to_string()));
            }
        }
    }
    kept.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut out: Vec<Segment> = Vec::with_capacity(kept.len());
    for (start, end, text) in kept {
        let mut start_ms = to_ms(start);
        if let Some(prev) = out.last() {
            start_ms = start_ms.max(prev.end_ms);
        }
        let end_ms = to_ms(end).max(start_ms);
        out.push(Segment {
            start_ms,
            end_ms,
            text,
        });
    }
    out
}

fn to_ms(sec: f64) -> u64 {
    (sec * 1000.0).round().max(0.0) as u64
}

/// Split into a new segment after a pause this long, or once a segment runs this long.
pub const SEGMENT_GAP_SEC: f64 = 0.8;
pub const SEGMENT_MAX_SEC: f64 = 18.0;
/// The recognizer reports when a token starts; a word ends a short tail after its last token.
const TOKEN_TAIL_SEC: f64 = 0.4;

fn has_letter_or_digit(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Group sub-word tokens (with their start times) into segments, the way the
/// hosted service does: a long pause or a long run starts a new segment.
/// Punctuation never starts a segment (CTC models often emit the full stop
/// after the pause that follows it), and a long run is only split where a new
/// word begins.
pub fn group_tokens_into_segments(tokens: &[String], timestamps: &[f64]) -> Vec<RelativeSegment> {
    let mut segs: Vec<RelativeSegment> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut seg_start = timestamps.first().copied().unwrap_or(0.0);
    let mut prev = seg_start;
    let flush =
        |cur: &mut Vec<&str>, segs: &mut Vec<RelativeSegment>, seg_start: f64, prev: f64| {
            let text = collapse_ws(&cur.concat());
            if !text.is_empty() {
                segs.push(RelativeSegment {
                    start: seg_start,
                    end: (prev + TOKEN_TAIL_SEC).max(seg_start),
                    text,
                });
            }
            cur.clear();
        };
    for (i, token) in tokens.iter().enumerate() {
        let t = timestamps
            .get(i)
            .copied()
            .filter(|t| t.is_finite())
            .unwrap_or(prev);
        if !cur.is_empty() && !has_letter_or_digit(token) {
            cur.push(token);
            continue;
        }
        let starts_word = token.chars().next().is_some_and(char::is_whitespace);
        if !cur.is_empty()
            && (t - prev > SEGMENT_GAP_SEC || (starts_word && t - seg_start > SEGMENT_MAX_SEC))
        {
            flush(&mut cur, &mut segs, seg_start, prev);
        }
        if cur.is_empty() {
            seg_start = t;
        }
        cur.push(token);
        prev = t;
    }
    flush(&mut cur, &mut segs, seg_start, prev);
    // The tail must not run into the next segment.
    for i in 0..segs.len().saturating_sub(1) {
        if segs[i].end > segs[i + 1].start {
            segs[i].end = segs[i].start.max(segs[i + 1].start);
        }
    }
    segs
}

/// The tokens a piece owns, on the recording's timeline: those whose start
/// falls in the piece's core. Merging at token level (rather than segment
/// level) means a sentence that runs across a cut is never kept twice.
pub fn tokens_in_core(
    plan: &ChunkPlan,
    tokens: &[String],
    timestamps: &[f64],
) -> Vec<(String, f64)> {
    tokens
        .iter()
        .zip(timestamps)
        .filter(|(_, rel)| rel.is_finite())
        .map(|(tok, rel)| (tok.clone(), rel + plan.extract_start_sec))
        .filter(|(_, t)| *t >= plan.keep_start_sec && *t < plan.keep_end_sec)
        .collect()
}

/// Segments that are already on the recording's timeline (seconds) as sorted, non-overlapping ms segments.
pub fn to_timeline(segments: Vec<RelativeSegment>) -> Vec<Segment> {
    merge_pieces(&[PieceResult {
        plan: ChunkPlan {
            index: 0,
            extract_start_sec: 0.0,
            extract_dur_sec: f64::INFINITY,
            keep_start_sec: f64::NEG_INFINITY,
            keep_end_sec: f64::INFINITY,
            hard_cut: false,
        },
        segments,
    }])
}

/// A word with its times in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedWord {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

fn ends_sentence(s: &str) -> bool {
    let t = s.trim_end_matches(['"', '\'', ')', ']']);
    t.ends_with(['.', '!', '?', '\u{3002}', '\u{FF01}', '\u{FF1F}'])
}

fn tidy_punctuation(s: &str) -> String {
    // " ," -> "," for , . ! ? ; : (the TS regex /\s+([,.!?;:])/g).
    let mut out = String::with_capacity(s.len());
    let mut pending_ws = String::new();
    for c in s.chars() {
        if c.is_whitespace() {
            pending_ws.push(c);
            continue;
        }
        if !matches!(c, ',' | '.' | '!' | '?' | ';' | ':') {
            out.push_str(&pending_ws);
        }
        pending_ws.clear();
        out.push(c);
    }
    out.push_str(&pending_ws);
    out
}

/// Group words into readable segments: break at a sentence end once a segment
/// has a few seconds in it, at a long pause, or once it runs too long.
pub fn group_words_into_segments(words: &[TimedWord], joiner: &str) -> Vec<RelativeSegment> {
    let mut segs = Vec::new();
    let mut cur: Vec<&TimedWord> = Vec::new();
    let flush = |cur: &mut Vec<&TimedWord>, segs: &mut Vec<RelativeSegment>| {
        if cur.is_empty() {
            return;
        }
        let joined = cur
            .iter()
            .map(|w| w.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(joiner);
        let text = tidy_punctuation(&joined).trim().to_string();
        if !text.is_empty() {
            let first = cur[0];
            let last = cur[cur.len() - 1];
            segs.push(RelativeSegment {
                start: first.start,
                end: last.end.max(first.start),
                text,
            });
        }
        cur.clear();
    };
    for w in words {
        if w.text.trim().is_empty() {
            continue;
        }
        if let Some(last) = cur.last() {
            if w.start - last.end > SEGMENT_GAP_SEC || w.end - cur[0].start > SEGMENT_MAX_SEC {
                flush(&mut cur, &mut segs);
            }
        }
        cur.push(w);
        if ends_sentence(w.text.trim()) && w.end - cur[0].start >= 3.0 {
            flush(&mut cur, &mut segs);
        }
    }
    flush(&mut cur, &mut segs);
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sil(a: f64, b: f64) -> SilenceSpan {
        SilenceSpan {
            start_sec: a,
            end_sec: b,
        }
    }

    const PARAKEET: ChunkPlanOptions = ChunkPlanOptions {
        target_sec: 130.0,
        max_sec: 180.0,
        overlap_sec: 2.5,
        min_sec: 15.0,
    };
    const WHISPER: ChunkPlanOptions = ChunkPlanOptions {
        target_sec: 24.0,
        max_sec: 28.0,
        overlap_sec: 0.0,
        min_sec: 5.0,
    };

    #[test]
    fn short_audio_is_one_piece() {
        let p = plan_chunks(&[], 120.0, &PARAKEET);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].extract_dur_sec, 120.0);
        assert!(p[0].keep_start_sec.is_infinite() && p[0].keep_end_sec.is_infinite());
        assert!(plan_chunks(&[], 0.0, &PARAKEET).is_empty());
    }

    #[test]
    fn cuts_land_in_silences_near_the_target() {
        let silences = [
            sil(100.0, 101.0),
            sil(128.0, 130.0),
            sil(250.0, 251.0),
            sil(262.0, 264.0),
        ];
        let p = plan_chunks(&silences, 400.0, &PARAKEET);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].keep_end_sec, 129.0);
        assert_eq!(p[1].keep_start_sec, 129.0);
        assert_eq!(p[1].keep_end_sec, 263.0);
        assert!(!p[0].hard_cut && !p[1].hard_cut);
        assert_eq!(p[1].extract_start_sec, 126.5);
        for c in &p {
            assert!(c.extract_dur_sec <= PARAKEET.max_sec + 1e-9, "{c:?}");
        }
    }

    #[test]
    fn no_silence_still_fits_the_hard_limit() {
        let total = 3600.0;
        let p = plan_chunks(&[], total, &PARAKEET);
        assert!(p.len() > 20);
        assert!(p.iter().all(|c| c.extract_dur_sec <= 180.0 + 1e-9));
        assert!(p.iter().all(|c| c.hard_cut));
        assert_eq!(
            p.last().unwrap().extract_start_sec + p.last().unwrap().extract_dur_sec,
            total
        );
        let w = plan_chunks(&[], 100.0, &WHISPER);
        assert!(w.iter().all(|c| c.extract_dur_sec <= 28.0 + 1e-9));
        // Whisper pieces do not overlap.
        for pair in w.windows(2) {
            assert!(
                (pair[0].extract_start_sec + pair[0].extract_dur_sec - pair[1].extract_start_sec)
                    .abs()
                    < 1e-9
            );
        }
    }

    #[test]
    fn speech_and_trim_follow_silences() {
        let s = [sil(0.0, 2.0), sil(8.0, 10.0)];
        assert_eq!(speech_sec_in(&s, 0.0, 10.0), 6.0);
        assert_eq!(trim_to_speech(&s, 0.0, 10.0), (2.0, 8.0));
        assert_eq!(speech_sec_in(&[sil(0.0, 10.0)], 0.0, 10.0), 0.0);
    }

    #[test]
    fn merge_keeps_overlap_once_and_never_overlaps() {
        let plans = plan_chunks(&[sil(129.0, 131.0)], 250.0, &PARAKEET);
        assert_eq!(plans.len(), 2);
        let seam = plans[0].keep_end_sec; // 130
        let a = PieceResult {
            plan: plans[0],
            segments: vec![
                RelativeSegment {
                    start: 120.0,
                    end: 129.0,
                    text: "first".into(),
                },
                RelativeSegment {
                    start: 129.6,
                    end: 131.0,
                    text: "dup".into(),
                }, // centre 130.3: belongs to piece 2
            ],
        };
        let off = plans[1].extract_start_sec; // 127.5
        let b = PieceResult {
            plan: plans[1],
            segments: vec![
                RelativeSegment {
                    start: 129.6 - off,
                    end: 131.0 - off,
                    text: "dup".into(),
                },
                RelativeSegment {
                    start: 128.0 - off,
                    end: 129.5 - off,
                    text: "early".into(),
                }, // centre 128.75: belongs to piece 1
            ],
        };
        assert_eq!(seam, 130.0);
        let m = merge_pieces(&[a, b]);
        let texts: Vec<_> = m.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "dup"]);
        assert_eq!(m[1].start_ms, 129_600);
    }

    #[test]
    fn merge_nudges_overlapping_segments() {
        let m = to_timeline(vec![
            RelativeSegment {
                start: 1.0,
                end: 3.0,
                text: " a ".into(),
            },
            RelativeSegment {
                start: 2.0,
                end: 2.5,
                text: "b".into(),
            },
            RelativeSegment {
                start: 4.0,
                end: 5.0,
                text: "  ".into(),
            },
        ]);
        assert_eq!(
            m,
            vec![
                Segment {
                    start_ms: 1000,
                    end_ms: 3000,
                    text: "a".into()
                },
                Segment {
                    start_ms: 3000,
                    end_ms: 3000,
                    text: "b".into()
                },
            ]
        );
    }

    fn toks(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn tokens_group_on_pauses_and_punctuation_stays() {
        let t = toks(&[" Hello", " world", ".", " Next", " one"]);
        let ts = [0.0, 0.3, 1.5, 1.6, 1.9];
        let g = group_tokens_into_segments(&t, &ts);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].text, "Hello world.");
        assert_eq!(g[1].text, "Next one");
        assert_eq!(g[0].end, 0.7); // 0.3 + tail, the "." did not move it
        assert!(g[0].end <= g[1].start);
    }

    #[test]
    fn long_runs_split_only_at_word_starts() {
        let mut t = Vec::new();
        let mut ts = Vec::new();
        for i in 0..50 {
            t.push(if i % 2 == 0 {
                " wo".to_string()
            } else {
                "rd".to_string()
            });
            ts.push(i as f64 * 0.5);
        }
        let g = group_tokens_into_segments(&t, &ts);
        assert!(g.len() >= 2);
        assert!(g
            .iter()
            .all(|s| s.text.starts_with("wo") && s.text.ends_with("rd")));
    }

    #[test]
    fn token_merge_across_a_cut_keeps_a_sentence_once() {
        let plans = plan_chunks(&[], 300.0, &PARAKEET);
        let seam = plans[0].keep_end_sec;
        let tokens = toks(&[" a", " b", " c"]);
        // The same three tokens seen by both pieces, straddling the seam.
        let abs = [seam - 1.0, seam - 0.5, seam + 0.2];
        let rel0: Vec<f64> = abs.iter().map(|t| t - plans[0].extract_start_sec).collect();
        let rel1: Vec<f64> = abs.iter().map(|t| t - plans[1].extract_start_sec).collect();
        let mut all = tokens_in_core(&plans[0], &tokens, &rel0);
        all.extend(tokens_in_core(&plans[1], &tokens, &rel1));
        all.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (tk, tt): (Vec<String>, Vec<f64>) = all.into_iter().unzip();
        let segs = to_timeline(group_tokens_into_segments(&tk, &tt));
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "a b c");
    }

    #[test]
    fn words_group_into_sentences() {
        let w = |s: f64, e: f64, t: &str| TimedWord {
            start: s,
            end: e,
            text: t.into(),
        };
        let words = vec![
            w(0.0, 0.5, "This"),
            w(0.5, 1.0, "is"),
            w(1.0, 3.2, "fine."),
            w(3.3, 3.6, "Then"),
            w(3.6, 3.9, ","),
            w(5.0, 5.5, "later"),
        ];
        let g = group_words_into_segments(&words, " ");
        let texts: Vec<_> = g.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["This is fine.", "Then,", "later"]);
    }
}

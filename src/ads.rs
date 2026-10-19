// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Pocketatui contributors

//! Finding ads.
//!
//! **Inserted ads** (skipped): many podcasts have ads stitched into each download by an
//! ad server, so your copy has audio the episode's own transcript doesn't. Whisper
//! transcribes your audio ahead of playback; every stretch of it is compared with the
//! episode's transcript by matching runs of four words. Speech that matches nowhere,
//! long enough and either pushing the rest of the episode later or sitting at the very
//! start or end, is audio that isn't part of the show: an inserted ad.
//!
//! **Sponsor hints** (only marked): ads the hosts read are in the transcript too, so
//! comparing can't find them. Phrases like "brought to you by" and "promo code" can, but
//! only roughly, so these are shown, never skipped.

use std::collections::HashMap;

use crate::transcript::{Cue, Word, words};

/// Words matched as a run (the same as auto-sync).
const N: usize = 4;
/// A match counts only if this many nearby runs agree on its offset.
const AGREE: usize = 2;
/// How far (in runs) to look for agreeing matches.
const NEAR: usize = 10;
/// The shortest ad worth skipping, in seconds.
const MIN_AD: f64 = 15.0;
/// Unmatched words needed when the episode's timing jumps across the gap.
const MIN_WORDS: usize = 15;
/// Seconds the rest of the episode must move later for a gap to count as inserted.
const MIN_DROP: f64 = 10.0;
/// Without a timing jump (an ad swapped for one of the same length), need more evidence.
const SWAP_SECS: f64 = 25.0;
const SWAP_WORDS: usize = 40;
/// Unmatched words needed at the start or end of the episode.
const EDGE_WORDS: usize = 20;

/// Stretches of the audio (seconds, audio time) that aren't in the episode's own
/// transcript.
///
/// `own` is the episode's transcript (its own times), `heard` is Whisper's transcript of
/// the audio being played, `covered` the stretches Whisper has done, and `audio_end` the
/// episode's length, once known.
pub fn find_inserted(
    own: &[Cue],
    heard: &[Cue],
    covered: &[(f64, f64)],
    audio_end: Option<f64>,
) -> Found {
    let tw = words(own);
    let hw = words(heard);
    let offsets = matched_offsets(&tw, &hw);
    let matched: Vec<usize> = (0..hw.len()).filter(|&i| offsets[i].is_some()).collect();
    let (Some(&first), Some(&last)) = (matched.first(), matched.last()) else {
        return Found::default();
    };
    let anchors = anchors(heard, &hw, &offsets, &matched);
    let cue_start = |w: &Word| heard[w.cue].start;
    let cue_end = |w: &Word| heard[w.cue].end;
    let mut out = Vec::new();

    // Before the show starts (a pre-roll).
    if first >= EDGE_WORDS && covers(covered, 0.0, hw[first].time) {
        out.push((0.0, cue_start(&hw[first])));
    }

    // Between two matched words.
    for pair in matched.windows(2) {
        let (p, n) = (pair[0], pair[1]);
        let between = n - p - 1;
        if between < MIN_WORDS || hw[p].cue == hw[n].cue {
            continue;
        }
        let (start, end) = (cue_end(&hw[p]), cue_start(&hw[n]));
        let secs = end - start;
        if secs < MIN_AD || !covers(covered, hw[p].time, hw[n].time) {
            continue;
        }
        let drop = offsets[p].unwrap_or(0.0) - offsets[n].unwrap_or(0.0);
        let inserted = drop >= MIN_DROP;
        let swapped = secs >= SWAP_SECS && between >= SWAP_WORDS;
        if inserted || swapped {
            out.push((start, end));
        }
    }

    // After the show ends (a post-roll).
    if let Some(end) = audio_end {
        let after = hw.len() - 1 - last;
        if after >= EDGE_WORDS && covers(covered, hw[last].time, end - 2.0) {
            out.push((cue_end(&hw[last]), end));
        }
    }

    out.retain(|(a, b)| b - a >= MIN_AD);
    Found { ads: out, anchors }
}

/// What comparing the two transcripts found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Found {
    /// Inserted ads, audio time.
    pub ads: Vec<(f64, f64)>,
    /// Where the episode's transcript lines up with the audio: `(audio time, offset)`,
    /// one wherever the offset changes (i.e. after each inserted ad).
    pub anchors: Vec<(f64, f64)>,
}

/// An anchor at the start of every matched stretch whose offset differs from the last.
fn anchors(heard: &[Cue], hw: &[Word], offsets: &[Option<f64>], matched: &[usize]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for &i in matched {
        let Some(off) = offsets[i] else { continue };
        if out.last().is_none_or(|&(_, last)| (off - last).abs() > 1.5) {
            out.push((heard[hw[i].cue].start, off));
        }
    }
    out
}

/// For each heard word, the transcript-minus-audio offset of a convincing match, if any.
fn matched_offsets(own: &[Word], heard: &[Word]) -> Vec<Option<f64>> {
    let mut out = vec![None; heard.len()];
    if heard.len() < N || own.len() < N {
        return out;
    }
    let key = |ws: &[Word]| {
        ws.iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut index: HashMap<String, Vec<f64>> = HashMap::new();
    for g in own.windows(N) {
        index.entry(key(g)).or_default().push(g[0].time);
    }
    // Every candidate offset of each run of heard words.
    let candidates: Vec<Vec<f64>> = heard
        .windows(N)
        .map(|g| {
            index
                .get(&key(g))
                .into_iter()
                .flatten()
                .map(|t| t - g[0].time)
                .collect()
        })
        .collect();
    for (i, cands) in candidates.iter().enumerate() {
        let lo = i.saturating_sub(NEAR);
        let hi = (i + NEAR).min(candidates.len() - 1);
        // The candidate the most neighbouring runs agree with.
        let best = cands
            .iter()
            .map(|&d| {
                let agree = (lo..=hi)
                    .filter(|&j| j != i)
                    .filter(|&j| candidates[j].iter().any(|e| (e - d).abs() <= 2.0))
                    .count();
                (agree, d)
            })
            .max_by_key(|(agree, _)| *agree);
        if let Some((agree, d)) = best
            && agree >= AGREE
        {
            for slot in &mut out[i..i + N] {
                slot.get_or_insert(d);
            }
        }
    }
    out
}

/// Whisper has transcribed all of `from..to`.
fn covers(covered: &[(f64, f64)], from: f64, to: f64) -> bool {
    covered.iter().any(|(a, b)| *a <= from + 1.0 && to <= *b)
}

const STRONG: &[&str] = &[
    "brought to you by",
    "sponsored by",
    "is sponsored",
    "our sponsor",
    "today's sponsor",
    "this week's sponsor",
    "promo code",
    "offer code",
    "discount code",
    "use code",
    "use the code",
    "support for this",
    "support for the show",
    "is supported by",
    "supported by",
    "thanks to our",
    "a word from",
];

const WEAK: &[&str] = &[
    ".com",
    "dot com",
    "slash",
    "percent off",
    "% off",
    "free trial",
    "free shipping",
    "sign up",
    "first month",
    "first order",
    "terms apply",
    "discount",
    "risk free",
    "risk-free",
    "money back",
    "download the app",
    "limited time",
    "special offer",
    "exclusive offer",
    "visit",
    "head to",
    "go to",
];

/// Hits this close together (seconds) belong to the same sponsor segment.
const HINT_GAP: f64 = 45.0;
/// Score a segment needs (strong phrases count 2, others 1), with at least one strong one.
const HINT_SCORE: u32 = 4;

/// Likely sponsor segments read by the hosts, as inclusive cue index ranges.
pub fn sponsor_hints(cues: &[Cue]) -> Vec<(usize, usize)> {
    let hits: Vec<(usize, u32, bool)> = cues
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let t = c.text.to_lowercase().replace('’', "'");
            let strong = STRONG.iter().filter(|p| t.contains(*p)).count() as u32;
            let weak = WEAK.iter().filter(|p| t.contains(*p)).count() as u32;
            let score = strong * 2 + weak;
            (score > 0).then_some((i, score, strong > 0))
        })
        .collect();
    let mut out = Vec::new();
    let mut group: Option<(usize, usize, u32, bool)> = None;
    for (i, score, strong) in hits {
        match &mut group {
            Some((_, last, total, any_strong)) if cues[i].start - cues[*last].end <= HINT_GAP => {
                *last = i;
                *total += score;
                *any_strong |= strong;
            }
            _ => {
                if let Some(g) = group.take() {
                    push_hint(&mut out, g);
                }
                group = Some((i, i, score, strong));
            }
        }
    }
    if let Some(g) = group {
        push_hint(&mut out, g);
    }
    out
}

fn push_hint(out: &mut Vec<(usize, usize)>, (first, last, score, strong): (usize, usize, u32, bool)) {
    if strong && score >= HINT_SCORE {
        out.push((first, last));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cues of `words_per_cue` words, 2.5 words a second, from `start`.
    fn cues(words: &[String], start: f64, per_cue: usize) -> Vec<Cue> {
        words
            .chunks(per_cue)
            .enumerate()
            .map(|(i, ws)| {
                let s = start + (i * per_cue) as f64 / 2.5;
                Cue {
                    start: s,
                    end: s + ws.len() as f64 / 2.5,
                    text: ws.join(" "),
                }
            })
            .collect()
    }

    /// `n` made-up words that don't repeat in runs of four.
    fn talk(seed: u64, n: usize) -> Vec<String> {
        const VOCAB: &[&str] = &[
            "the", "race", "was", "a", "mess", "and", "then", "he", "pitted", "for", "hards",
            "which", "honestly", "made", "no", "sense", "to", "me", "but", "strategy", "is",
            "weird", "this", "year", "with", "tyres", "degrading", "fast", "in", "heat",
        ];
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                VOCAB[(x >> 33) as usize % VOCAB.len()].to_string()
            })
            .collect()
    }

    /// Shift cues later by `by` seconds.
    fn shifted(mut c: Vec<Cue>, by: f64) -> Vec<Cue> {
        for cue in &mut c {
            cue.start += by;
            cue.end += by;
        }
        c
    }

    #[test]
    fn finds_an_inserted_ad_and_a_preroll() {
        let show = talk(1, 1500); // 10 minutes
        let own = cues(&show, 0.0, 12);
        let ad = talk(99, 150); // 60 s of other speech
        let pre = talk(7, 75); // 30 s pre-roll

        // Heard: pre-roll, first 600 words of the show, the ad, the rest.
        let mut heard = cues(&pre, 0.0, 12);
        heard.extend(shifted(cues(&show[..600], 0.0, 12), 30.0));
        heard.extend(cues(&ad, 30.0 + 240.0, 12));
        heard.extend(shifted(cues(&show[600..], 240.0, 12), 90.0));
        let total = 30.0 + 600.0 + 60.0;

        let found = find_inserted(&own, &heard, &[(0.0, total)], Some(total));
        let ads = found.ads;
        assert_eq!(ads.len(), 2, "{ads:?}");
        let (a, b) = ads[0];
        assert!(a == 0.0 && (b - 30.0).abs() < 5.0, "pre-roll {ads:?}");
        let (a, b) = ads[1];
        assert!((a - 270.0).abs() < 5.0 && (b - 330.0).abs() < 5.0, "mid-roll {ads:?}");
        // The transcript lines up 30 s late after the pre-roll, 90 s late after the ad.
        let offs: Vec<f64> = found.anchors.iter().map(|a| a.1.round()).collect();
        assert_eq!(offs, vec![-30.0, -90.0], "{:?}", found.anchors);
        assert!((found.anchors[1].0 - 330.0).abs() < 5.0, "{:?}", found.anchors);
    }

    #[test]
    fn leaves_the_show_alone() {
        let show = talk(3, 1500);
        let own = cues(&show, 0.0, 12);
        // Same speech, a different cue split, and every 9th word misheard.
        let mut heard_words = show.clone();
        for (i, w) in heard_words.iter_mut().enumerate() {
            if i % 9 == 4 {
                *w = "uh".into();
            }
        }
        let heard = shifted(cues(&heard_words, 0.0, 9), 1.0);
        assert!(find_inserted(&own, &heard, &[(0.0, 601.0)], Some(601.0)).ads.is_empty());
    }

    #[test]
    fn ignores_short_gaps_and_unscanned_audio() {
        let show = talk(5, 1500);
        let own = cues(&show, 0.0, 12);
        let blip = talk(42, 20); // 8 s of something else
        let mut heard = cues(&show[..600], 0.0, 12);
        heard.extend(cues(&blip, 240.0, 12));
        heard.extend(shifted(cues(&show[600..], 240.0, 12), 8.0));
        assert!(find_inserted(&own, &heard, &[(0.0, 608.0)], None).ads.is_empty());

        // A real 60 s ad, but Whisper hasn't covered the stretch yet.
        let ad = talk(43, 150);
        let mut heard = cues(&show[..600], 0.0, 12);
        heard.extend(cues(&ad, 240.0, 12));
        heard.extend(shifted(cues(&show[600..], 240.0, 12), 60.0));
        assert!(
            find_inserted(&own, &heard, &[(0.0, 250.0), (300.0, 660.0)], None)
                .ads
                .is_empty()
        );
        assert_eq!(find_inserted(&own, &heard, &[(0.0, 660.0)], None).ads.len(), 1);
    }

    #[test]
    fn hints_at_sponsor_reads() {
        let line = |s: f64, text: &str| Cue {
            start: s,
            end: s + 5.0,
            text: text.into(),
        };
        let c = vec![
            line(0.0, "Welcome back to the show."),
            line(5.0, "Lots to get through today."),
            line(60.0, "This episode is brought to you by Acme."),
            line(70.0, "Acme makes great socks."),
            line(80.0, "Go to acme dot com slash show and use code SHOW for 20 percent off."),
            line(90.0, "Anyway, back to the race."),
            line(300.0, "I'd visit Monza again."),
        ];
        assert_eq!(sponsor_hints(&c), vec![(2, 4)]);
        // A lone mention isn't enough.
        let c = vec![line(0.0, "They were sponsored by a bank back then.")];
        assert!(sponsor_hints(&c).is_empty());
    }
}

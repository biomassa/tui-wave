pub mod auto_trim;
pub mod cdp;
pub mod cut;
pub mod delete;
pub mod fade;
pub mod gain;
pub mod head_tail_mark;
pub mod high_pass;
pub mod marker;
pub mod normalize;
pub mod paste;
pub mod remove_channels;
pub mod remove_dc;
pub mod resample;
pub mod reverse;
pub mod trim;

/// Every command's answer to `Command::is_noop` and `Command::stored_bytes`. The costly mistake
/// is a command that changed the audio but calls itself a no-op: `History` would drop it and
/// the edit could not be undone. So each command is run once on a real change and once on a
/// change that does nothing.
#[cfg(test)]
mod undo_accounting_tests {
    use crate::model::command::Command;
    use crate::model::document::Document;
    use crate::model::dsp::DcEstimator;

    /// Two channels of 1000 samples: a ramp with a +0.1 bias, so every command has something
    /// to act on.
    fn audio() -> Document {
        let mut doc = crate::model::io::load_wav("tests/fixtures/mono_sine.wav").unwrap();
        let ramp: Vec<f32> = (0..1000).map(|i| 0.1 + i as f32 / 2000.0).collect();
        doc.channels = vec![ramp.clone(), ramp];
        doc.markers.clear();
        doc.head_tail_marks.clear();
        doc.original_channels.clear();
        doc
    }

    fn silence() -> Document {
        let mut doc = audio();
        doc.channels = vec![vec![0.0; 1000]; 2];
        doc
    }

    /// Runs `cmd` on `doc` and returns (is_noop, stored_bytes, whether the audio changed).
    fn run(mut cmd: Box<dyn Command>, mut doc: Document) -> (bool, usize, bool) {
        let before = doc.channels.clone();
        cmd.execute(&mut doc);
        (cmd.is_noop(), cmd.stored_bytes(), doc.channels != before)
    }

    /// 100 samples of both channels, in bytes.
    const RANGE_100: usize = 100 * 2 * 4;

    #[test]
    fn a_real_edit_is_never_a_noop_and_counts_its_stored_samples() {
        let cases: Vec<(&str, Box<dyn Command>, usize)> = vec![
            ("gain", super::gain::gain_command(100, 200, vec![-6.0], false), RANGE_100),
            ("normalize", super::normalize::normalize_command(100, 200, -1.0), RANGE_100),
            ("fade", super::fade::fade_command(100, 200, true, super::fade::FadeCurve::Linear), RANGE_100),
            ("high-pass", super::high_pass::high_pass_command(100, 200, 1000.0), RANGE_100),
            ("reverse", super::reverse::reverse_command(100, 200), 0),
            ("delete", super::delete::delete_command(100..200), RANGE_100),
            ("paste", super::paste::paste_command(100, vec![vec![0.5; 100]; 2]), RANGE_100),
            ("trim", super::trim::trim_command(100, 200), (1000 - 100) * 2 * 4),
            ("remove dc", super::remove_dc::remove_dc_command(DcEstimator::default()), 0),
            ("remove channel", super::remove_channels::remove_channels_command(vec![1]), 1000 * 4),
            ("resample", super::resample::resample_command(22050), 1000 * 2 * 4),
            ("technical fades", super::fade::technical_fades_command(50), 2 * 50 * 2 * 4),
            ("auto-trim", super::auto_trim::auto_trim_command((100, 900), vec![(400, 500)], 10), 0),
        ];
        for (name, cmd, want_bytes) in cases {
            let (noop, bytes, changed) = run(cmd, audio());
            assert!(changed, "{name}: the case must really change the audio");
            assert!(!noop, "{name}: changed the audio but called itself a no-op");
            if name == "auto-trim" {
                assert!(bytes > 0, "{name}: holds the trimmed audio");
            } else {
                assert_eq!(bytes, want_bytes, "{name}: stored bytes");
            }
        }
    }

    #[test]
    fn an_edit_that_changes_nothing_is_a_noop() {
        let cases: Vec<(&str, Box<dyn Command>, Document)> = vec![
            ("normalize on silence", super::normalize::normalize_command(0, 1000, -1.0), silence()),
            ("gain on an empty range", super::gain::gain_command(100, 100, vec![-6.0], false), audio()),
            ("fade on one sample", super::fade::fade_command(100, 101, true, super::fade::FadeCurve::Linear), audio()),
            ("high-pass above Nyquist", super::high_pass::high_pass_command(0, 1000, 1.0e9), audio()),
            ("reverse of an empty range", super::reverse::reverse_command(100, 100), audio()),
            ("delete of an empty range", super::delete::delete_command(100..100), audio()),
            ("paste of nothing", super::paste::paste_command(100, vec![Vec::new(); 2]), audio()),
            ("trim past the end", super::trim::trim_command(100, 5000), audio()),
            ("remove dc on silence", super::remove_dc::remove_dc_command(DcEstimator::default()), silence()),
            ("remove a channel that is not there", super::remove_channels::remove_channels_command(vec![9]), audio()),
        ];
        for (name, cmd, doc) in cases {
            let (noop, _, changed) = run(cmd, doc);
            assert!(!changed, "{name}: the case must change nothing");
            assert!(noop, "{name}: changed nothing but kept an undo step");
        }
    }
}

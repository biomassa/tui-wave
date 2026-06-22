/// Lives outside `Document` (owned by the editor session) so paste can eventually target a
/// different open document, not just the one it was cut from.
#[derive(Default)]
pub struct Clipboard {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() || self.channels.iter().all(|c| c.is_empty())
    }

    pub fn set(&mut self, channels: Vec<Vec<f32>>, sample_rate: u32) {
        self.channels = channels;
        self.sample_rate = sample_rate;
    }
}

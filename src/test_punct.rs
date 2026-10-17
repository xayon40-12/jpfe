
use std::borrow::Cow;
use lindera::dictionary::{load_embedded_dictionary, DictionaryKind};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;

fn main() -> anyhow::Result<()> {
    let dictionary = load_embedded_dictionary(DictionaryKind::IPADIC)?;
    let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
    let sample = "「――――　………　―――　……　！？　!?　……っ！　ー　ーーー　〜〜〜";
    let mut tokens = segmenter.segment(Cow::Borrowed(sample))?;
    for t in tokens.iter_mut() {
        println!("surface: {:<10} pos: {:<10} details: {:?}", t.surface, t.details()[0], t.details());
    }
    Ok(())
}

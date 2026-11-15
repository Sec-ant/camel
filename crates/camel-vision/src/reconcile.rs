use camel::{Rect, TextBlock, Word};

/// Keep one observation of each word and discard clipped fragments covered by
/// a longer agreeing observation on the same measured baseline.
pub(crate) fn reconcile(blocks: &mut Vec<TextBlock>) {
    let removals: Vec<Vec<bool>> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            block
                .words
                .iter()
                .map(|word| {
                    blocks.iter().enumerate().any(|(j, other)| {
                        i != j
                            && other.words.iter().any(|full| {
                                fragment(word, full)
                                    || subglyph_fragment(word, full, block, other)
                                    || (overlapping_duplicate(word, full)
                                        && (line_support(other, full) > line_support(block, word)
                                            || (line_support(other, full)
                                                == line_support(block, word)
                                                && j < i)))
                            })
                    })
                })
                .collect()
        })
        .collect();
    for (block, remove) in blocks.iter_mut().zip(removals) {
        if !remove.iter().any(|&v| v) {
            continue;
        }
        let mut index = 0;
        block.words.retain(|_| {
            let keep = !remove[index];
            index += 1;
            keep
        });
        block.bounds = block
            .words
            .iter()
            .map(|w| w.bounds)
            .reduce(Rect::union)
            .unwrap_or_default();
        block.text = block
            .words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    blocks.retain(|b| !b.words.is_empty());
    let mut i = 0;
    while i < blocks.len() {
        if blocks[..i]
            .iter()
            .any(|other| same_words(&blocks[i], other))
        {
            blocks.remove(i);
        } else {
            i += 1;
        }
    }
}

fn overlapping_duplicate(a: &Word, b: &Word) -> bool {
    if a.text.is_empty() || !a.text.eq_ignore_ascii_case(&b.text) {
        return false;
    }
    let a = a.bounds;
    let b = b.bounds;
    let overlap = u128::from(a.right().min(b.right()).saturating_sub(a.x.max(b.x)))
        * u128::from(a.bottom().min(b.bottom()).saturating_sub(a.y.max(b.y)));
    let smaller = (u128::from(a.width) * u128::from(a.height))
        .min(u128::from(b.width) * u128::from(b.height));
    smaller > 0 && overlap * 5 >= smaller * 4
}

fn subglyph_fragment(part: &Word, full: &Word, block: &TextBlock, other: &TextBlock) -> bool {
    let a = part.bounds;
    let b = full.bounds;
    let mut chars = part.text.chars();
    let Some(c) = chars.next() else { return false };
    chars.next().is_none()
        && full.text.chars().filter(|c| c.is_alphanumeric()).count() >= 2
        && (c.is_alphanumeric() || full.text.ends_with(c))
        && a.width > 0
        && a.height > 0
        && u64::from(a.width) * 2 <= u64::from(b.width)
        && u64::from(a.height) * 2 <= u64::from(b.height)
        && a.x.saturating_add(a.width / 2) >= b.x
        && a.x.saturating_add(a.width / 2) < b.right()
        && a.y.saturating_add(a.height / 2) >= b.y
        && a.y.saturating_add(a.height / 2) < b.bottom()
        && line_support(other, full) > line_support(block, part)
}

fn line_support(block: &TextBlock, word: &Word) -> usize {
    block
        .words
        .iter()
        .filter(|other| other.line == word.line)
        .count()
}

fn fragment(part: &Word, full: &Word) -> bool {
    let a = part.bounds;
    let b = full.bounds;
    !part.text.is_empty()
        && part.text.len() < full.text.len()
        && a.y == b.y
        && a.height == b.height
        && a.width > 0
        && a.width < b.width
        && a.x >= b.x
        && a.right() <= b.right()
        && (full
            .text
            .get(..part.text.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(&part.text))
            || full
                .text
                .get(full.text.len() - part.text.len()..)
                .is_some_and(|s| s.eq_ignore_ascii_case(&part.text)))
}

fn same_words(a: &TextBlock, b: &TextBlock) -> bool {
    a.words.len() == b.words.len()
        && a.words
            .iter()
            .zip(&b.words)
            .all(|(a, b)| a.bounds == b.bounds && a.text.eq_ignore_ascii_case(&b.text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u32, words: &[(&str, u32, u32, u32)]) -> TextBlock {
        let words: Vec<_> = words
            .iter()
            .map(|&(text, x, y, width)| Word {
                text: text.into(),
                bounds: Rect {
                    x,
                    y,
                    width,
                    height: 20,
                },
                confidence: if id == 0 { 99.9 } else { 0.1 },
                line: y,
            })
            .collect();
        TextBlock {
            id,
            bounds: words.iter().map(|w| w.bounds).reduce(Rect::union).unwrap(),
            text: words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            words,
        }
    }

    #[test]
    fn clipped_neighbor_is_removed_without_comparing_scores() {
        let mut blocks = vec![
            block(0, &[("SOON.", 10, 50, 50), ("TEL", 81, 50, 39)]),
            block(1, &[("TELL", 80, 50, 55), ("ME.", 85, 80, 40)]),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks[0].text, "SOON.");
        assert_eq!(blocks[0].bounds, blocks[0].words[0].bounds);
        assert_eq!(blocks[1].text, "TELL ME.");
        assert_eq!(blocks[1].id, 1);
    }

    #[test]
    fn duplicate_observations_do_not_remove_repeated_dialogue_elsewhere() {
        let mut blocks = vec![
            block(0, &[("YES!", 10, 10, 40)]),
            block(1, &[("YES!", 10, 10, 40)]),
            block(2, &[("YES!", 10, 70, 40)]),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks.iter().map(|b| b.id).collect::<Vec<_>>(), [0, 2]);
    }

    #[test]
    fn overlapping_detections_keep_unique_words_without_repeating_shared_words() {
        let mut blocks = vec![
            block(0, &[("DARKWING", 10, 10, 90), ("SEQUENCE", 110, 10, 90)]),
            block(
                1,
                &[
                    ("DARKWING", 10, 10, 90),
                    ("SEQUENCE", 110, 10, 90),
                    ("RED", 10, 40, 30),
                    ("RUSH", 50, 40, 40),
                ],
            ),
            block(2, &[("DARKWING", 10, 100, 90)]),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks[0].text, "DARKWING SEQUENCE");
        assert_eq!(blocks[1].text, "RED RUSH");
        assert_eq!(blocks[1].bounds.y, 40);
        assert_eq!(blocks[2].text, "DARKWING");
        reconcile(&mut blocks);
        assert_eq!(blocks[1].text, "RED RUSH");
    }

    #[test]
    fn overlapping_crops_reconcile_offset_words_and_clipped_header_fragments() {
        let mut header = block(
            0,
            &[
                ("IMAGE", 10, 10, 100),
                ("COMICS,", 120, 10, 100),
                ("INC.", 230, 10, 60),
                ("Robert", 10, 40, 51),
                ("chief", 140, 40, 41),
            ],
        );
        header.words[3].bounds.height = 19;
        header.words[4].bounds.height = 12;
        let mut body = block(
            1,
            &[
                ("y", 195, 22, 5),
                (".", 275, 22, 4),
                ("Robert", 10, 40, 52),
                ("Kirkman", 68, 40, 60),
                ("chief", 149, 40, 32),
                ("officer", 190, 40, 50),
            ],
        );
        body.words[0].bounds.height = 8;
        body.words[1].bounds.height = 5;
        for word in &mut body.words[2..] {
            word.bounds.height = 12;
        }
        let mut blocks = vec![header, body];
        reconcile(&mut blocks);
        assert_eq!(blocks[0].text, "IMAGE COMICS, INC.");
        assert_eq!(blocks[1].text, "Robert Kirkman chief officer");
        reconcile(&mut blocks);
        assert_eq!(blocks[0].text, "IMAGE COMICS, INC.");
    }

    #[test]
    fn independent_punctuation_is_not_discarded_as_a_word_fragment() {
        let mut header = block(0, &[("HELLO", 10, 10, 60), ("YOU", 80, 10, 40)]);
        let mut body = block(1, &[("!", 60, 20, 3)]);
        body.words[0].bounds.height = 6;
        header.words[0].bounds.height = 20;
        let mut blocks = vec![header, body];
        reconcile(&mut blocks);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].text, "!");
    }

    #[test]
    fn duplicated_word_stays_with_the_more_complete_line() {
        let mut blocks = vec![
            block(0, &[("YOU", 10, 40, 40), ("HELLO", 10, 10, 50)]),
            block(
                1,
                &[
                    ("YOU", 10, 40, 40),
                    ("SAVED", 60, 40, 60),
                    ("HIM", 130, 40, 40),
                ],
            ),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks[0].text, "HELLO");
        assert_eq!(blocks[1].text, "YOU SAVED HIM");
        reconcile(&mut blocks);
        assert_eq!(blocks[1].text, "YOU SAVED HIM");
    }

    #[test]
    fn conflicting_readings_and_separate_baselines_are_retained() {
        let mut blocks = vec![
            block(0, &[("TEL", 10, 10, 40)]),
            block(1, &[("SELL", 10, 10, 55)]),
            block(2, &[("TELL", 10, 40, 55)]),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks.len(), 3);
    }

    #[test]
    fn fragment_chains_end_at_one_complete_observation() {
        let mut blocks = vec![
            block(0, &[("TE", 10, 10, 25)]),
            block(1, &[("TEL", 10, 10, 40)]),
            block(2, &[("TELL", 10, 10, 55)]),
            block(3, &[("TELL", 10, 10, 55)]),
        ];
        reconcile(&mut blocks);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "TELL");
        assert_eq!(blocks[0].id, 2);
        reconcile(&mut blocks);
        assert_eq!(blocks.len(), 1);
    }
}

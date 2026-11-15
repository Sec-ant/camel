#![expect(
    clippy::unused_async_trait_impl,
    reason = "the test transport answers without waiting"
)]

use camel::translation::{Context, TranslationError, Translator, translate_page};
use camel::{Bubble, Page, Rect, Region, TextBlock};
use futures::executor::block_on;
use std::collections::BTreeMap;

struct Reply(&'static str);

impl Translator for Reply {
    async fn complete(&self, _: &str, _: &str) -> Result<String, TranslationError> {
        Ok(self.0.into())
    }
}

fn page() -> Page {
    Page {
        width: 100,
        height: 100,
        regions: vec![Region {
            block: TextBlock {
                id: 1,
                text: "Hello, Mark!".into(),
                words: vec![],
                bounds: Rect::default(),
            },
            bubble: Bubble {
                bounds: Rect::default(),
                interior: vec![],
                glyphs: vec![],
                erase: vec![],
                background: [255; 3],
                background_model: None,
                background_repair: None,
                background_issue: None,
            },
            translation: None,
            font_size: None,
        }],
    }
}

#[test]
fn model_term_explanations_preserve_translation_and_confirmed_names() {
    let mut page = page();
    let mut context = Context::default();
    context
        .confirm_terms(&BTreeMap::from([("Mark".into(), "马克".into())]))
        .unwrap();
    let conflicts = block_on(translate_page(
        &Reply(r#"{"translations":[{"id":1,"text":"你好，马克！"}],"terms":[{"source":"Mark","target":"马可","note":"alternate spelling"},{"source":"Eve","target":"伊芙","note":"character"}]}"#),
        &mut page, None, &mut context, "zh-CN",
    )).unwrap();
    assert_eq!(page.regions[0].translation.as_deref(), Some("你好，马克！"));
    assert_eq!(context.glossary["Mark"], "马克");
    assert!(!context.proposed_terms.contains_key("Mark"));
    assert_eq!(context.proposed_terms["Eve"], "伊芙");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].confirmed_target, "马克");
    assert_eq!(conflicts[0].proposed_target, "马可");
}

#[test]
fn invalid_terms_and_region_ids_are_still_rejected_atomically() {
    for raw in [
        r#"{"translations":[{"id":1,"text":"你好"}],"terms":[{"target":"马克","note":"missing source"}]}"#,
        r#"{"translations":[{"id":1,"text":"你好"}],"terms":[{"source":"Mark","note":"missing target"}]}"#,
        r#"{"translations":[{"id":1,"text":"你好"}],"terms":[{"source":" ","target":"马克","note":"blank source"}]}"#,
        r#"{"translations":[{"id":1,"text":"你好"}],"terms":[{"source":"Mark","target":" ","note":"blank target"}]}"#,
        r#"{"translations":[{"id":1,"text":"你好","note":"unexpected region field"}],"terms":[{"source":"Mark","target":"马克","note":"valid term"}]}"#,
        r#"{"translations":[{"id":1,"text":"你好"},{"id":1,"text":"再见"}],"terms":[{"source":"Mark","target":"马克","note":"duplicate id"}]}"#,
        r#"{"translations":[],"terms":[{"source":"Mark","target":"马克","note":"missing id"}]}"#,
    ] {
        let mut page = page();
        let mut context = Context::default();
        assert!(
            block_on(translate_page(
                &Reply(raw),
                &mut page,
                None,
                &mut context,
                "zh-CN"
            ))
            .is_err(),
            "{raw}"
        );
        assert!(page.regions[0].translation.is_none());
        assert!(context.proposed_terms.is_empty());
        assert!(context.recent.is_empty());
    }
}

use holtmail_core::Timestamp;
use holtmail_insight::limits::{
    MAX_ANCHORS, MAX_HTML_BYTES, MAX_PKPASS_ATTACHMENTS, MAX_PKPASS_BYTES, MAX_SEGMENTS,
    MAX_TEXT_BYTES,
};
use holtmail_insight::{AttachmentMeta, HeaderField, HtmlView, InsightInput};
use holtmail_mime::{Attachment, ParsedMessage};

use crate::text::{NormalizedText, prefix};

const PKPASS_TYPE: &str = "application/vnd.apple.pkpass";

pub(crate) fn build(message: &ParsedMessage) -> (InsightInput, bool) {
    let mut truncated = false;

    let text = NormalizedText::new(message.text.as_deref().unwrap_or(""), MAX_TEXT_BYTES);
    truncated |= text.truncated;
    let forward_at = holtmail_html_scan::forward_at(&text.text);

    let html = message.html.as_deref().map(|html| {
        let kept = prefix(html, MAX_HTML_BYTES);
        truncated |= kept.len() < html.len();
        let view = holtmail_html_scan::scan(kept);
        truncated |= view_at_cap(&view);
        view
    });

    let mut pkpass_kept = 0;
    let mut attachments = Vec::new();
    for (ordinal, attachment) in message.attachments.iter().enumerate() {
        let Ok(ordinal) = u16::try_from(ordinal) else {
            truncated = true;
            break;
        };
        let content = if is_pkpass(attachment) {
            if attachment.content.len() > MAX_PKPASS_BYTES || pkpass_kept >= MAX_PKPASS_ATTACHMENTS
            {
                truncated = true;
                None
            } else {
                pkpass_kept += 1;
                Some(attachment.content.clone())
            }
        } else {
            None
        };
        attachments.push(AttachmentMeta {
            ordinal,
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.to_ascii_lowercase(),
            size: attachment.content.len() as u64,
            content,
        });
    }

    let input = InsightInput {
        headers: message
            .headers
            .iter()
            .map(|field| HeaderField::new(&field.name, &field.value))
            .collect(),
        from: message.from.clone(),
        reply_to: message.reply_to.clone(),
        subject: message.subject.clone(),
        sent_at: message.sent_at.map(Timestamp::from_secs),
        text: text.text,
        forward_at,
        html,
        attachments,
    };
    (input, truncated)
}

fn view_at_cap(view: &HtmlView) -> bool {
    view.anchors.len() >= MAX_ANCHORS
        || view.segments.len() >= MAX_SEGMENTS
        || view.truncated_structured
}

fn is_pkpass(attachment: &Attachment) -> bool {
    attachment.content_type.eq_ignore_ascii_case(PKPASS_TYPE)
        || attachment.filename.as_deref().is_some_and(|name| {
            name.len() > 7
                && name.is_char_boundary(name.len() - 7)
                && name[name.len() - 7..].eq_ignore_ascii_case(".pkpass")
        })
}

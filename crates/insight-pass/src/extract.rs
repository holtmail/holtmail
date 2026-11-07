use holtmail_insight::limits::{MAX_ITEMS_PER_EXTRACTOR, MAX_PKPASS_ATTACHMENTS};
use holtmail_insight::{
    Confidence, Explanation, Insight, InsightInput, InsightKind, PartRef, Provenance,
};

use crate::archive::read_pass_json;
use crate::pass;

pub(crate) struct Log(Option<Vec<Explanation>>);

impl Log {
    pub(crate) fn off() -> Self {
        Self(None)
    }

    pub(crate) fn on() -> Self {
        Self(Some(Vec::new()))
    }

    pub(crate) fn into_steps(self) -> Vec<Explanation> {
        self.0.unwrap_or_default()
    }

    fn step(&mut self, ordinal: u16, rule: &'static str, count: Option<usize>) {
        if let Some(steps) = &mut self.0 {
            let mut step = Explanation::new(rule).part(PartRef::Attachment(ordinal));
            if let Some(count) = count {
                step = step.count(count);
            }
            steps.push(step);
        }
    }
}

pub(crate) fn extract(input: &InsightInput, log: &mut Log) -> Vec<Insight> {
    let mut items = Vec::new();
    let mut seen: Vec<u16> = Vec::new();
    for attachment in &input.attachments {
        let Some(content) = &attachment.content else {
            continue;
        };
        let ordinal = attachment.ordinal;
        if seen.len() >= MAX_PKPASS_ATTACHMENTS || items.len() >= MAX_ITEMS_PER_EXTRACTOR {
            log.step(ordinal, "attachments_capped", Some(seen.len()));
            break;
        }
        if seen.contains(&ordinal) {
            log.step(ordinal, "duplicate_ordinal", None);
            continue;
        }
        seen.push(ordinal);
        log.step(ordinal, "attachment", Some(content.len()));

        let json = match read_pass_json(content) {
            Ok(json) => json,
            Err((refusal, entries)) => {
                log.step(ordinal, refusal.rule(), Some(entries));
                continue;
            }
        };
        log.step(ordinal, "entries", Some(json.entries));
        log.step(ordinal, "pass_json", Some(json.bytes.len()));

        let pass = match pass::read(&json.bytes) {
            Ok(pass) => pass,
            Err(rejection) => {
                log.step(ordinal, rejection.rule(), None);
                continue;
            }
        };
        log.step(ordinal, style_rule(pass.style), None);
        if pass.relevant_present {
            log.step(
                ordinal,
                "relevant_date",
                Some(usize::from(pass.relevant.is_some())),
            );
        }
        log.step(ordinal, "semantics", Some(usize::from(pass.semantics)));
        log.step(ordinal, "barcodes", Some(pass.barcodes));
        log.step(
            ordinal,
            pass.boarding_outcome.rule(),
            pass.boarding.as_ref().map(|boarding| boarding.legs.len()),
        );
        log.step(ordinal, "chosen", None);

        items.push(Insight {
            kind: InsightKind::Pass {
                style: pass.style,
                organization: pass.organization,
                description: pass.description,
                relevant: pass.relevant,
                boarding: pass.boarding,
                attachment: ordinal,
                signature_checked: false,
            },
            provenance: Provenance::PkPass,
            confidence: Confidence::Certain,
            score: 1000,
            origin: PartRef::Attachment(ordinal),
            forwarded: false,
        });
    }
    items
}

fn style_rule(style: holtmail_insight::PassStyle) -> &'static str {
    use holtmail_insight::PassStyle;
    match style {
        PassStyle::BoardingPass => "style:boarding_pass",
        PassStyle::EventTicket => "style:event_ticket",
        PassStyle::Coupon => "style:coupon",
        PassStyle::StoreCard => "style:store_card",
        PassStyle::Generic => "style:generic",
    }
}

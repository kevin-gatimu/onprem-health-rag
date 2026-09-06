//! Tier 0 — the conversational gate.
//!
//! A front-of-pipeline, model-free classifier that recognises messages needing
//! **no retrieval at all**: greetings, thanks/acknowledgements, farewells,
//! identity/capability questions, and obviously out-of-domain requests. Ported
//! from the Electron app's proven anchored-regex design
//! (`On-premise-Rag-system-for-Health-Records/docs/intent-router.md`).
//!
//! Two design rules keep precision high:
//! 1. **Anchored, whole-message matching** for greetings/acks/identity — so a
//!    real query that merely *starts* with a greeting word ("hey how many
//!    patients…") is NOT swallowed by the gate; only a message that IS the
//!    greeting matches.
//! 2. **A meta-query negative guard** — questions *about the conversation*
//!    ("what have I asked?", "recap our chat") need the ordered transcript, so
//!    they must fall through to the semantic path, never the gate.
//!
//! The gate fails open: no match → `false` → the normal pipeline runs.

use std::sync::LazyLock;

use regex::Regex;

// Greetings: the whole message is a greeting (optionally with a short vocative
// like "there"/"team" and trailing punctuation). Anchored to end so
// "hey, how many patients?" does NOT match.
static GREETING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(hi+|hello+|hey+|hiya|yo|howdy|greetings|good\s+(morning|afternoon|evening|day))(\s+(there|all|everyone|team|assistant|bot|folks))?\s*[.!,]*\s*$",
    )
    .unwrap()
});

// Thanks / short acknowledgements. Whole-message only, so "ok, now list all
// patients" falls through to the pipeline.
static ACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(thanks|thank\s+you|thank\s+u|thx|ty|ok|okay|k|cool|great|got\s+it|nice|awesome|perfect|sounds\s+good|no\s+problem|np|cheers|much\s+appreciated|appreciate\s+it|understood)\s*[.!]*\s*$",
    )
    .unwrap()
});

// Farewells. Whole-message, end-anchored.
static FAREWELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(bye|goodbye|good\s*bye|see\s+(you|ya)( later)?|farewell|good\s*night|catch\s+you\s+later)\s*[.!]*\s*$",
    )
    .unwrap()
});

// Identity / capability questions about the assistant itself. End-anchored so a
// clinical question that happens to open with "what is this…" (e.g. "what is
// this patient's diagnosis") is not misrouted.
static IDENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(who\s+are\s+you|what\s+are\s+you|what\s+can\s+you\s+do|what\s+do\s+you\s+do|what\s+is\s+this(\s+(app|tool|system|thing))?|introduce\s+yourself|tell\s+me\s+about\s+yourself|how\s+do\s+you\s+work|what\s+can\s+i\s+ask(\s+you)?(\s+about)?|help)\s*[?.!]*\s*$",
    )
    .unwrap()
});

// Obviously out-of-domain requests (creative writing, weather, small talk).
// Prefix-anchored — these openings are unambiguous and never begin a clinical
// query. Kept deliberately narrow: a wrong route here costs a real answer.
static OUT_OF_DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(write\s+(me\s+)?an?\s+(poem|song|story|joke|essay|rap|haiku)|tell\s+me\s+a\s+joke|sing\s+(me\s+)?a?\s*song|what('?s| is)\s+the\s+weather|what\s+time\s+is\s+it|who\s+won\s+the|translate\s+this)",
    )
    .unwrap()
});

// Negative guard: questions about the conversation itself. These need the
// ordered transcript (semantic/history path), so they must NOT be gated.
static META: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(what\s+(have|did)\s+i\s+(asked|ask|said|say)|recap|summar(y|ise|ize)\s+(our|this|the)\s+(chat|conversation)|our\s+conversation|earlier\s+i\s+(asked|said)|my\s+(first|last|previous)\s+(question|message)|repeat\s+(my|the)\b|what\s+was\s+my)",
    )
    .unwrap()
});

/// True when the message needs no retrieval — a greeting, thanks, farewell,
/// identity/capability question, or an obvious out-of-domain request. Returns
/// `false` for conversation-meta questions (they need the transcript) and for
/// anything the anchored patterns don't confidently recognise (fail-open).
pub fn is_conversational(message: &str) -> bool {
    // Meta questions about the conversation are never gated.
    if is_conversation_meta(message) {
        return false;
    }
    GREETING.is_match(message)
        || ACK.is_match(message)
        || FAREWELL.is_match(message)
        || IDENTITY.is_match(message)
        || OUT_OF_DOMAIN.is_match(message)
}

/// True when the message asks about the conversation itself ("what have I
/// asked?", "recap our chat"). Exposed for the negative guard and its tests.
pub fn is_conversation_meta(message: &str) -> bool {
    META.is_match(message)
}

// Capability questions: "what can you do", "what data do you have", "help".
// A strict subset of the identity/capability wording that can be answered
// *factually* from the schema binding rather than improvised by a model.
// End-anchored for the same reason `IDENTITY` is: "what data do you have on
// Jane Chebet" is a records question, not a capability question.
static CAPABILITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(what\s+can\s+you\s+do|what\s+do\s+you\s+do|what\s+can\s+i\s+ask(\s+you)?(\s+about)?|what\s+(data|tables|records|information|info)\s+(do\s+you|can\s+you)\s+(have|see|access|read)|what\s+(data|information)\s+is\s+available|what\s+are\s+you\s+able\s+to\s+do|help|what\s+questions\s+can\s+i\s+ask)\s*[?.!]*\s*$",
    )
    .unwrap()
});

/// True when the message is a **capability** question ("what can you do?",
/// "what data do you have?", "help") — answerable from the schema binding with
/// no model call (plan 05 §5).
///
/// Deliberately a separate predicate from [`is_conversational`], which keeps its
/// existing behaviour and fixtures: this one is consulted *first* by `route_v3`
/// and by the agents endpoint, so a capability question gets the truthful,
/// binding-derived answer instead of a model's guess. Conversation-meta
/// questions are excluded for the same reason they are excluded there.
pub fn is_capability(message: &str) -> bool {
    if is_conversation_meta(message) {
        return false;
    }
    CAPABILITY.is_match(message)
}

// ---------------------------------------------------------------------------
// Tests — ≥20 positive / ≥20 negative fixtures, per the plan's Phase A gate.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan 05 §5: capability questions are recognised as their own class.
    /// `is_conversational` keeps its existing behaviour — these fixtures assert
    /// the new predicate only.
    #[test]
    fn capability_questions_are_recognised() {
        for yes in [
            "what can you do?",
            "What can you do",
            "what data do you have?",
            "What tables can you see?",
            "what can I ask you about?",
            "help",
            "What questions can I ask?",
        ] {
            assert!(is_capability(yes), "expected capability: {yes:?}");
        }
        for no in [
            "what data do you have on Jane Chebet",
            "help me find patient records",
            "how many deliveries last month",
            "what have I asked so far?",
            "hello",
        ] {
            assert!(!is_capability(no), "expected NOT capability: {no:?}");
        }
    }

    #[test]
    fn positive_conversational_fixtures() {
        let yes = [
            "hi",
            "Hello there",
            "hey",
            "heyy",
            "good morning",
            "Good afternoon!",
            "greetings",
            "thanks",
            "thank you!",
            "thx",
            "ok",
            "great",
            "got it",
            "cool.",
            "no problem",
            "bye",
            "goodbye",
            "see you later",
            "who are you",
            "what can you do?",
            "what are you?",
            "help",
            "introduce yourself",
            "write me a poem about the ocean",
            "tell me a joke",
            "what's the weather today",
        ];
        for m in yes {
            assert!(is_conversational(m), "expected conversational: {m:?}");
        }
    }

    #[test]
    fn negative_clinical_and_meta_fixtures() {
        let no = [
            "how many patients have diabetes",
            "list all patients",
            "summarize the records for John Doe",
            "hey how many diabetic patients are there",
            "explain what hypertension means",
            "trend of malaria cases over time",
            "who are the patients with diabetes",
            "what is the average age of patients",
            "show all encounters",
            "find patient P-001234",
            "thanks, now how many patients are there",
            "ok so tell me about the diabetic cohort",
            "what is this patient's diagnosis",
            "distribution of patients by blood type",
            "write a discharge summary for patient 5",
            "help me find patient records",
            "what have I asked so far?",
            "recap our conversation",
            "what did I ask earlier",
            "summarise this chat",
            "what was my first question",
            "count of prescriptions per clinic",
            "breakdown by gender",
        ];
        for m in no {
            assert!(!is_conversational(m), "expected NOT conversational: {m:?}");
        }
    }

    #[test]
    fn meta_guard_overrides_identity() {
        // Even if an identity-ish phrasing appears, a meta cue keeps it off the gate.
        assert!(is_conversation_meta("what have i asked"));
        assert!(!is_conversational("what have i asked"));
    }
}

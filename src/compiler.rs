//! Turn events into a string of HTML.
use crate::constant::{SAFE_PROTOCOL_HREF, SAFE_PROTOCOL_SRC};
use crate::construct::character_reference::Kind as CharacterReferenceKind;
use crate::token::Token;
use crate::tokenizer::{Code, Event, EventType};
use crate::util::normalize_identifier::normalize_identifier;
use crate::util::{
    decode_character_reference::{decode_named, decode_numeric},
    encode::encode,
    sanitize_uri::sanitize_uri,
    skip,
    span::{codes as codes_from_span, from_exit_event, serialize},
};
use crate::{LineEnding, Options};

/// Representation of a link or image, resource or reference.
/// Reused for temporary definitions as well, in the first pass.
#[derive(Debug)]
struct Media {
    /// Whether this represents an image (`true`) or a link or definition
    /// (`false`).
    image: bool,
    /// The text between the brackets (`x` in `![x]()` and `[x]()`), as an
    /// identifier, meaning that the original source characters are used
    /// instead of interpreting them.
    /// Not interpreted.
    label_id: Option<String>,
    /// The text between the brackets (`x` in `![x]()` and `[x]()`), as
    /// interpreted content.
    /// When this is a link, it can contain further text content and thus HTML
    /// tags.
    /// Otherwise, when an image, text content is also allowed, but resulting
    /// tags are ignored.
    label: Option<String>,
    /// The text between the explicit brackets of the reference (`y` in
    /// `[x][y]`), as content.
    /// Not interpreted.
    reference_id: Option<String>,
    /// The destination (url).
    /// Interpreted string content.
    destination: Option<String>,
    /// The destination (url).
    /// Interpreted string content.
    title: Option<String>,
}

/// Representation of a definition.
#[derive(Debug)]
struct Definition {
    /// The destination (url).
    /// Interpreted string content.
    destination: Option<String>,
    /// The title.
    /// Interpreted string content.
    title: Option<String>,
}

/// Context used to compile markdown.
#[allow(clippy::struct_excessive_bools)]
struct CompileContext<'a> {
    /// Static info.
    pub events: &'a [Event],
    pub codes: &'a [Code],
    /// Fields used by handlers to track the things they need to track to
    /// compile markdown.
    pub atx_opening_sequence_size: Option<usize>,
    pub heading_setext_buffer: Option<String>,
    pub code_flow_seen_data: Option<bool>,
    pub code_fenced_fences_count: Option<usize>,
    pub code_text_inside: bool,
    pub character_reference_kind: Option<CharacterReferenceKind>,
    pub expect_first_item: Option<bool>,
    pub media_stack: Vec<Media>,
    pub definitions: Vec<(String, Definition)>,
    pub tight_stack: Vec<bool>,
    /// Fields used to influance the current compilation.
    pub slurp_one_line_ending: bool,
    pub tags: bool,
    pub ignore_encode: bool,
    pub last_was_tag: bool,
    /// Configuration
    pub protocol_href: Option<Vec<&'static str>>,
    pub protocol_src: Option<Vec<&'static str>>,
    pub line_ending_default: LineEnding,
    pub allow_dangerous_html: bool,
    /// Intermediate results.
    pub buffers: Vec<String>,
    pub index: usize,
}

impl<'a> CompileContext<'a> {
    /// Create a new compile context.
    pub fn new(
        events: &'a [Event],
        codes: &'a [Code],
        options: &Options,
        line_ending: LineEnding,
    ) -> CompileContext<'a> {
        CompileContext {
            events,
            codes,
            atx_opening_sequence_size: None,
            heading_setext_buffer: None,
            code_flow_seen_data: None,
            code_fenced_fences_count: None,
            code_text_inside: false,
            character_reference_kind: None,
            expect_first_item: None,
            media_stack: vec![],
            definitions: vec![],
            tight_stack: vec![],
            slurp_one_line_ending: false,
            tags: true,
            ignore_encode: false,
            last_was_tag: false,
            protocol_href: if options.allow_dangerous_protocol {
                None
            } else {
                Some(SAFE_PROTOCOL_HREF.to_vec())
            },
            protocol_src: if options.allow_dangerous_protocol {
                None
            } else {
                Some(SAFE_PROTOCOL_SRC.to_vec())
            },
            line_ending_default: line_ending,
            allow_dangerous_html: options.allow_dangerous_html,
            buffers: vec![String::new()],
            index: 0,
        }
    }

    /// Push a buffer.
    pub fn buffer(&mut self) {
        self.buffers.push(String::new());
    }

    /// Pop a buffer, returning its value.
    pub fn resume(&mut self) -> String {
        self.buffers.pop().expect("Cannot resume w/o buffer")
    }

    pub fn push<'x, S: Into<&'x str>>(&mut self, value: S) {
        let value = value.into();
        self.buffers
            .last_mut()
            .expect("Cannot push w/o buffer")
            .push_str(value);
        self.last_was_tag = false;
    }

    pub fn push_raw<'x, S: Into<&'x str>>(&mut self, value: S) {
        let value = value.into();
        if self.ignore_encode {
            self.push(value);
        } else {
            self.push(&*encode(value));
        }
    }

    pub fn tag<'x, S: Into<&'x str>>(&mut self, value: S) {
        if self.tags {
            self.push(value.into());
            self.last_was_tag = true;
        }
    }

    /// Get the current buffer.
    pub fn buf_tail(&self) -> &String {
        self.buffers
            .last()
            .expect("at least one buffer should exist")
    }

    /// Add a line ending.
    pub fn line_ending(&mut self) {
        let eol = self.line_ending_default.as_str().to_string();
        self.push(&*eol);
    }

    /// Add a line ending if needed (as in, there’s no eol/eof already).
    pub fn line_ending_if_needed(&mut self) {
        let last_char = self.buf_tail().chars().last();
        let mut add = true;

        if let Some(x) = last_char {
            if x == '\n' || x == '\r' {
                add = false;
            }
        } else {
            add = false;
        }

        if add {
            self.line_ending();
        }
    }
}

/// Turn events and codes into a string of HTML.
#[allow(clippy::too_many_lines)]
pub fn compile(events: &[Event], codes: &[Code], options: &Options) -> String {
    let mut index = 0;
    let mut line_ending_inferred: Option<LineEnding> = None;

    // First, we figure out what the used line ending style is.
    // Stop when we find a line ending.
    while index < events.len() {
        let event = &events[index];

        if event.event_type == EventType::Exit
            && (event.token_type == Token::BlankLineEnding || event.token_type == Token::LineEnding)
        {
            let codes = codes_from_span(codes, &from_exit_event(events, index));
            line_ending_inferred = Some(LineEnding::from_code(*codes.first().unwrap()));
            break;
        }

        index += 1;
    }

    // Figure out which line ending style we’ll use.
    let line_ending_default = if let Some(value) = line_ending_inferred {
        value
    } else {
        options.default_line_ending.clone()
    };

    // Handle one event.
    let handle = |context: &mut CompileContext, index: usize| {
        let event = &events[index];

        context.index = index;

        if event.event_type == EventType::Enter {
            enter(context);
        } else {
            exit(context);
        }
    };

    let mut context = CompileContext::new(events, codes, options, line_ending_default);
    let mut definition_indices: Vec<(usize, usize)> = vec![];
    let mut index = 0;
    let mut definition_inside = false;

    // Handle all definitions first.
    // We must do two passes because we need to compile the events in
    // definitions which come after references already.
    //
    // To speed things up, we collect the places we can jump over for the
    // second pass.
    while index < events.len() {
        let event = &events[index];

        if definition_inside {
            handle(&mut context, index);
        }

        if event.token_type == Token::Definition {
            if event.event_type == EventType::Enter {
                handle(&mut context, index); // Also handle start.
                definition_inside = true;
                definition_indices.push((index, index));
            } else {
                definition_inside = false;
                definition_indices.last_mut().unwrap().1 = index;
            }
        }

        index += 1;
    }

    index = 0;
    let jump_default = (events.len(), events.len());
    let mut definition_index = 0;
    let mut jump = definition_indices
        .get(definition_index)
        .unwrap_or(&jump_default);

    while index < events.len() {
        if index == jump.0 {
            index = jump.1 + 1;
            definition_index += 1;
            jump = definition_indices
                .get(definition_index)
                .unwrap_or(&jump_default);
            // Ignore line endings after definitions.
            context.slurp_one_line_ending = true;
        } else {
            handle(&mut context, index);
            index += 1;
        }
    }

    assert_eq!(context.buffers.len(), 1, "expected 1 final buffer");
    context
        .buffers
        .get(0)
        .expect("expected 1 final buffer")
        .to_string()
}

/// Handle [`Enter`][EventType::Enter].
fn enter(context: &mut CompileContext) {
    match context.events[context.index].token_type {
        Token::CodeFencedFenceInfo
        | Token::CodeFencedFenceMeta
        | Token::DefinitionLabelString
        | Token::DefinitionTitleString
        | Token::HeadingAtxText
        | Token::HeadingSetextText
        | Token::Label
        | Token::ReferenceString
        | Token::ResourceTitleString => on_enter_buffer(context),

        Token::BlockQuote => on_enter_block_quote(context),
        Token::CodeIndented => on_enter_code_indented(context),
        Token::CodeFenced => on_enter_code_fenced(context),
        Token::CodeText => on_enter_code_text(context),
        Token::Definition => on_enter_definition(context),
        Token::DefinitionDestinationString => on_enter_definition_destination_string(context),
        Token::Emphasis => on_enter_emphasis(context),
        Token::HtmlFlow => on_enter_html_flow(context),
        Token::HtmlText => on_enter_html_text(context),
        Token::Image => on_enter_image(context),
        Token::Link => on_enter_link(context),
        Token::ListItemMarker => on_enter_list_item_marker(context),
        Token::ListOrdered | Token::ListUnordered => on_enter_list(context),
        Token::Paragraph => on_enter_paragraph(context),
        Token::Resource => on_enter_resource(context),
        Token::ResourceDestinationString => on_enter_resource_destination_string(context),
        Token::Strong => on_enter_strong(context),
        _ => {}
    }
}

/// Handle [`Exit`][EventType::Exit].
fn exit(context: &mut CompileContext) {
    match context.events[context.index].token_type {
        Token::CodeFencedFenceMeta | Token::Resource => on_exit_drop(context),
        Token::CharacterEscapeValue | Token::CodeTextData | Token::Data => on_exit_data(context),

        Token::AutolinkEmail => on_exit_autolink_email(context),
        Token::AutolinkProtocol => on_exit_autolink_protocol(context),
        Token::BlankLineEnding => on_exit_blank_line_ending(context),
        Token::BlockQuote => on_exit_block_quote(context),
        Token::CharacterReferenceMarker => on_exit_character_reference_marker(context),
        Token::CharacterReferenceMarkerNumeric => {
            on_exit_character_reference_marker_numeric(context);
        }
        Token::CharacterReferenceMarkerHexadecimal => {
            on_exit_character_reference_marker_hexadecimal(context);
        }
        Token::CharacterReferenceValue => on_exit_character_reference_value(context),
        Token::CodeFenced | Token::CodeIndented => on_exit_code_flow(context),
        Token::CodeFencedFence => on_exit_code_fenced_fence(context),
        Token::CodeFencedFenceInfo => on_exit_code_fenced_fence_info(context),
        Token::CodeFlowChunk => on_exit_code_flow_chunk(context),
        Token::CodeText => on_exit_code_text(context),
        Token::Definition => on_exit_definition(context),
        Token::DefinitionDestinationString => on_exit_definition_destination_string(context),
        Token::DefinitionLabelString => on_exit_definition_label_string(context),
        Token::DefinitionTitleString => on_exit_definition_title_string(context),
        Token::Emphasis => on_exit_emphasis(context),
        Token::HardBreakEscape | Token::HardBreakTrailing => on_exit_break(context),
        Token::HeadingAtx => on_exit_heading_atx(context),
        Token::HeadingAtxSequence => on_exit_heading_atx_sequence(context),
        Token::HeadingAtxText => on_exit_heading_atx_text(context),
        Token::HeadingSetextText => on_exit_heading_setext_text(context),
        Token::HeadingSetextUnderline => on_exit_heading_setext_underline(context),
        Token::HtmlFlow | Token::HtmlText => on_exit_html(context),
        Token::HtmlFlowData | Token::HtmlTextData => on_exit_html_data(context),
        Token::Image | Token::Link => on_exit_media(context),
        Token::Label => on_exit_label(context),
        Token::LabelText => on_exit_label_text(context),
        Token::LineEnding => on_exit_line_ending(context),
        Token::ListOrdered | Token::ListUnordered => on_exit_list(context),
        Token::ListItem => on_exit_list_item(context),
        Token::ListItemValue => on_exit_list_item_value(context),
        Token::Paragraph => on_exit_paragraph(context),
        Token::ReferenceString => on_exit_reference_string(context),
        Token::ResourceDestinationString => on_exit_resource_destination_string(context),
        Token::ResourceTitleString => on_exit_resource_title_string(context),
        Token::Strong => on_exit_strong(context),
        Token::ThematicBreak => on_exit_thematic_break(context),
        _ => {}
    }
}

/// Handle [`Enter`][EventType::Enter]:`*`.
///
/// Buffers data.
fn on_enter_buffer(context: &mut CompileContext) {
    context.buffer();
}

/// Handle [`Enter`][EventType::Enter]:[`BlockQuote`][Token::BlockQuote].
fn on_enter_block_quote(context: &mut CompileContext) {
    context.tight_stack.push(false);
    context.line_ending_if_needed();
    context.tag("<blockquote>");
}

/// Handle [`Enter`][EventType::Enter]:[`CodeIndented`][Token::CodeIndented].
fn on_enter_code_indented(context: &mut CompileContext) {
    context.code_flow_seen_data = Some(false);
    context.line_ending_if_needed();
    context.tag("<pre><code>");
}

/// Handle [`Enter`][EventType::Enter]:[`CodeFenced`][Token::CodeFenced].
fn on_enter_code_fenced(context: &mut CompileContext) {
    context.code_flow_seen_data = Some(false);
    context.line_ending_if_needed();
    // Note that no `>` is used, which is added later.
    context.tag("<pre><code");
    context.code_fenced_fences_count = Some(0);
}

/// Handle [`Enter`][EventType::Enter]:[`CodeText`][Token::CodeText].
fn on_enter_code_text(context: &mut CompileContext) {
    context.code_text_inside = true;
    context.tag("<code>");
    context.buffer();
}

/// Handle [`Enter`][EventType::Enter]:[`Definition`][Token::Definition].
fn on_enter_definition(context: &mut CompileContext) {
    context.buffer();
    context.media_stack.push(Media {
        image: false,
        label: None,
        label_id: None,
        reference_id: None,
        destination: None,
        title: None,
    });
}

/// Handle [`Enter`][EventType::Enter]:[`DefinitionDestinationString`][Token::DefinitionDestinationString].
fn on_enter_definition_destination_string(context: &mut CompileContext) {
    context.buffer();
    context.ignore_encode = true;
}

/// Handle [`Enter`][EventType::Enter]:[`Emphasis`][Token::Emphasis].
fn on_enter_emphasis(context: &mut CompileContext) {
    context.tag("<em>");
}

/// Handle [`Enter`][EventType::Enter]:[`HtmlFlow`][Token::HtmlFlow].
fn on_enter_html_flow(context: &mut CompileContext) {
    context.line_ending_if_needed();
    if context.allow_dangerous_html {
        context.ignore_encode = true;
    }
}

/// Handle [`Enter`][EventType::Enter]:[`HtmlText`][Token::HtmlText].
fn on_enter_html_text(context: &mut CompileContext) {
    if context.allow_dangerous_html {
        context.ignore_encode = true;
    }
}

/// Handle [`Enter`][EventType::Enter]:[`Image`][Token::Image].
fn on_enter_image(context: &mut CompileContext) {
    context.media_stack.push(Media {
        image: true,
        label_id: None,
        label: None,
        reference_id: None,
        destination: None,
        title: None,
    });
    context.tags = false; // Disallow tags.
}

/// Handle [`Enter`][EventType::Enter]:[`Link`][Token::Link].
fn on_enter_link(context: &mut CompileContext) {
    context.media_stack.push(Media {
        image: false,
        label_id: None,
        label: None,
        reference_id: None,
        destination: None,
        title: None,
    });
}

/// Handle [`Enter`][EventType::Enter]:{[`ListOrdered`][Token::ListOrdered],[`ListUnordered`][Token::ListUnordered]}.
fn on_enter_list(context: &mut CompileContext) {
    let events = &context.events;
    let mut index = context.index;
    let mut balance = 0;
    let mut loose = false;
    let token_type = &events[index].token_type;

    while index < events.len() {
        let event = &events[index];

        if event.event_type == EventType::Enter {
            balance += 1;
        } else {
            balance -= 1;

            // Blank line directly in list or directly in list item,
            // but not a blank line after an empty list item.
            if balance < 3 && event.token_type == Token::BlankLineEnding {
                let at_marker = balance == 2
                    && events[skip::opt_back(
                        events,
                        index - 2,
                        &[Token::BlankLineEnding, Token::SpaceOrTab],
                    )]
                    .token_type
                        == Token::ListItemPrefix;
                let at_list_item = balance == 1 && events[index - 2].token_type == Token::ListItem;
                let at_empty_list_item = if at_list_item {
                    let before_item = skip::opt_back(events, index - 2, &[Token::ListItem]);
                    let before_prefix = skip::opt_back(
                        events,
                        index - 3,
                        &[Token::ListItemPrefix, Token::SpaceOrTab],
                    );
                    before_item + 1 == before_prefix
                } else {
                    false
                };

                if !at_marker && !at_list_item && !at_empty_list_item {
                    loose = true;
                    break;
                }
            }

            // Done.
            if balance == 0 && event.token_type == *token_type {
                break;
            }
        }

        index += 1;
    }

    context.tight_stack.push(!loose);
    context.line_ending_if_needed();
    // Note: no `>`.
    context.tag(&*format!(
        "<{}",
        if *token_type == Token::ListOrdered {
            "ol"
        } else {
            "ul"
        }
    ));
    context.expect_first_item = Some(true);
}

/// Handle [`Enter`][EventType::Enter]:[`ListItemMarker`][Token::ListItemMarker].
fn on_enter_list_item_marker(context: &mut CompileContext) {
    let expect_first_item = context.expect_first_item.take().unwrap();

    if expect_first_item {
        context.tag(">");
    }

    context.line_ending_if_needed();
    context.tag("<li>");
    context.expect_first_item = Some(false);
    // “Hack” to prevent a line ending from showing up if the item is empty.
    context.last_was_tag = false;
}

/// Handle [`Enter`][EventType::Enter]:[`Paragraph`][Token::Paragraph].
fn on_enter_paragraph(context: &mut CompileContext) {
    let tight = context.tight_stack.last().unwrap_or(&false);

    if !tight {
        context.line_ending_if_needed();
        context.tag("<p>");
    }
}

/// Handle [`Enter`][EventType::Enter]:[`Resource`][Token::Resource].
fn on_enter_resource(context: &mut CompileContext) {
    context.buffer(); // We can have line endings in the resource, ignore them.
    let media = context.media_stack.last_mut().unwrap();
    media.destination = Some("".to_string());
}

/// Handle [`Enter`][EventType::Enter]:[`ResourceDestinationString`][Token::ResourceDestinationString].
fn on_enter_resource_destination_string(context: &mut CompileContext) {
    context.buffer();
    // Ignore encoding the result, as we’ll first percent encode the url and
    // encode manually after.
    context.ignore_encode = true;
}

/// Handle [`Enter`][EventType::Enter]:[`Strong`][Token::Strong].
fn on_enter_strong(context: &mut CompileContext) {
    context.tag("<strong>");
}

/// Handle [`Exit`][EventType::Exit]:[`AutolinkEmail`][Token::AutolinkEmail].
fn on_exit_autolink_email(context: &mut CompileContext) {
    let slice = serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    );
    context.tag(&*format!(
        "<a href=\"{}\">",
        sanitize_uri(
            format!("mailto:{}", slice.as_str()).as_str(),
            &context.protocol_href
        )
    ));
    context.push_raw(&*slice);
    context.tag("</a>");
}

/// Handle [`Exit`][EventType::Exit]:[`AutolinkProtocol`][Token::AutolinkProtocol].
fn on_exit_autolink_protocol(context: &mut CompileContext) {
    let slice = serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    );
    context.tag(&*format!(
        "<a href=\"{}\">",
        sanitize_uri(slice.as_str(), &context.protocol_href)
    ));
    context.push_raw(&*slice);
    context.tag("</a>");
}

/// Handle [`Exit`][EventType::Exit]:{[`HardBreakEscape`][Token::HardBreakEscape],[`HardBreakTrailing`][Token::HardBreakTrailing]}.
fn on_exit_break(context: &mut CompileContext) {
    context.tag("<br />");
}

/// Handle [`Exit`][EventType::Exit]:[`BlankLineEnding`][Token::BlankLineEnding].
fn on_exit_blank_line_ending(context: &mut CompileContext) {
    if context.index == context.events.len() - 1 {
        context.line_ending_if_needed();
    }
}

/// Handle [`Exit`][EventType::Exit]:[`BlockQuote`][Token::BlockQuote].
fn on_exit_block_quote(context: &mut CompileContext) {
    context.tight_stack.pop();
    context.line_ending_if_needed();
    context.slurp_one_line_ending = false;
    context.tag("</blockquote>");
}

/// Handle [`Exit`][EventType::Exit]:[`CharacterReferenceMarker`][Token::CharacterReferenceMarker].
fn on_exit_character_reference_marker(context: &mut CompileContext) {
    context.character_reference_kind = Some(CharacterReferenceKind::Named);
}

/// Handle [`Exit`][EventType::Exit]:[`CharacterReferenceMarkerHexadecimal`][Token::CharacterReferenceMarkerHexadecimal].
fn on_exit_character_reference_marker_hexadecimal(context: &mut CompileContext) {
    context.character_reference_kind = Some(CharacterReferenceKind::Hexadecimal);
}

/// Handle [`Exit`][EventType::Exit]:[`CharacterReferenceMarkerNumeric`][Token::CharacterReferenceMarkerNumeric].
fn on_exit_character_reference_marker_numeric(context: &mut CompileContext) {
    context.character_reference_kind = Some(CharacterReferenceKind::Decimal);
}

/// Handle [`Exit`][EventType::Exit]:[`CharacterReferenceValue`][Token::CharacterReferenceValue].
fn on_exit_character_reference_value(context: &mut CompileContext) {
    let kind = context
        .character_reference_kind
        .take()
        .expect("expected `character_reference_kind` to be set");
    let reference = serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    );
    let ref_string = reference.as_str();
    let value = match kind {
        CharacterReferenceKind::Decimal => decode_numeric(ref_string, 10).to_string(),
        CharacterReferenceKind::Hexadecimal => decode_numeric(ref_string, 16).to_string(),
        CharacterReferenceKind::Named => decode_named(ref_string),
    };

    context.push_raw(&*value);
}

/// Handle [`Exit`][EventType::Exit]:[`CodeFlowChunk`][Token::CodeFlowChunk].
fn on_exit_code_flow_chunk(context: &mut CompileContext) {
    context.code_flow_seen_data = Some(true);
    context.push_raw(&*serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    ));
}

/// Handle [`Exit`][EventType::Exit]:[`CodeFencedFence`][Token::CodeFencedFence].
fn on_exit_code_fenced_fence(context: &mut CompileContext) {
    let count = if let Some(count) = context.code_fenced_fences_count {
        count
    } else {
        0
    };

    if count == 0 {
        context.tag(">");
        context.slurp_one_line_ending = true;
    }

    context.code_fenced_fences_count = Some(count + 1);
}

/// Handle [`Exit`][EventType::Exit]:[`CodeFencedFenceInfo`][Token::CodeFencedFenceInfo].
fn on_exit_code_fenced_fence_info(context: &mut CompileContext) {
    let value = context.resume();
    context.tag(&*format!(" class=\"language-{}\"", value));
}

/// Handle [`Exit`][EventType::Exit]:{[`CodeFenced`][Token::CodeFenced],[`CodeIndented`][Token::CodeIndented]}.
fn on_exit_code_flow(context: &mut CompileContext) {
    let seen_data = context
        .code_flow_seen_data
        .take()
        .expect("`code_flow_seen_data` must be defined");

    // One special case is if we are inside a container, and the fenced code was
    // not closed (meaning it runs to the end).
    // In that case, the following line ending, is considered *outside* the
    // fenced code and block quote by micromark, but CM wants to treat that
    // ending as part of the code.
    if let Some(count) = context.code_fenced_fences_count {
        if count == 1 && !context.tight_stack.is_empty() && !context.last_was_tag {
            context.line_ending();
        }
    }

    // But in most cases, it’s simpler: when we’ve seen some data, emit an extra
    // line ending when needed.
    if seen_data {
        context.line_ending_if_needed();
    }

    context.tag("</code></pre>");

    if let Some(count) = context.code_fenced_fences_count.take() {
        if count < 2 {
            context.line_ending_if_needed();
        }
    }

    context.slurp_one_line_ending = false;
}

/// Handle [`Exit`][EventType::Exit]:[`CodeText`][Token::CodeText].
fn on_exit_code_text(context: &mut CompileContext) {
    let result = context.resume();
    let mut chars = result.chars();
    let mut trim = false;

    if Some(' ') == chars.next() && Some(' ') == chars.next_back() {
        let mut next = chars.next();
        while next != None && !trim {
            if Some(' ') != next {
                trim = true;
            }
            next = chars.next();
        }
    }

    context.code_text_inside = false;
    context.push(&*if trim {
        result[1..(result.len() - 1)].to_string()
    } else {
        result
    });
    context.tag("</code>");
}

/// Handle [`Exit`][EventType::Exit]:*.
///
/// Resumes, and ignores what was resumed.
fn on_exit_drop(context: &mut CompileContext) {
    context.resume();
}

/// Handle [`Exit`][EventType::Exit]:{[`CodeTextData`][Token::CodeTextData],[`Data`][Token::Data],[`CharacterEscapeValue`][Token::CharacterEscapeValue]}.
fn on_exit_data(context: &mut CompileContext) {
    // Just output it.
    context.push_raw(&*serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    ));
}

/// Handle [`Exit`][EventType::Exit]:[`Definition`][Token::Definition].
fn on_exit_definition(context: &mut CompileContext) {
    let definition = context.media_stack.pop().unwrap();
    let reference_id = normalize_identifier(&definition.reference_id.unwrap());
    let destination = definition.destination;
    let title = definition.title;

    context.resume();

    let mut index = 0;

    while index < context.definitions.len() {
        if context.definitions[index].0 == reference_id {
            return;
        }

        index += 1;
    }

    context
        .definitions
        .push((reference_id, Definition { destination, title }));
}

/// Handle [`Exit`][EventType::Exit]:[`DefinitionDestinationString`][Token::DefinitionDestinationString].
fn on_exit_definition_destination_string(context: &mut CompileContext) {
    let buf = context.resume();
    let definition = context.media_stack.last_mut().unwrap();
    definition.destination = Some(buf);
    context.ignore_encode = false;
}

/// Handle [`Exit`][EventType::Exit]:[`DefinitionLabelString`][Token::DefinitionLabelString].
fn on_exit_definition_label_string(context: &mut CompileContext) {
    // Discard label, use the source content instead.
    context.resume();
    let definition = context.media_stack.last_mut().unwrap();
    definition.reference_id = Some(serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    ));
}

/// Handle [`Exit`][EventType::Exit]:[`DefinitionTitleString`][Token::DefinitionTitleString].
fn on_exit_definition_title_string(context: &mut CompileContext) {
    let buf = context.resume();
    let definition = context.media_stack.last_mut().unwrap();
    definition.title = Some(buf);
}

/// Handle [`Exit`][EventType::Exit]:[`Strong`][Token::Emphasis].
fn on_exit_emphasis(context: &mut CompileContext) {
    context.tag("</em>");
}

/// Handle [`Exit`][EventType::Exit]:[`HeadingAtx`][Token::HeadingAtx].
fn on_exit_heading_atx(context: &mut CompileContext) {
    let rank = context
        .atx_opening_sequence_size
        .take()
        .expect("`atx_opening_sequence_size` must be set in headings");

    context.tag(&*format!("</h{}>", rank));
}

/// Handle [`Exit`][EventType::Exit]:[`HeadingAtxSequence`][Token::HeadingAtxSequence].
fn on_exit_heading_atx_sequence(context: &mut CompileContext) {
    // First fence we see.
    if context.atx_opening_sequence_size.is_none() {
        let rank = serialize(
            context.codes,
            &from_exit_event(context.events, context.index),
            false,
        )
        .len();
        context.line_ending_if_needed();
        context.atx_opening_sequence_size = Some(rank);
        context.tag(&*format!("<h{}>", rank));
    }
}

/// Handle [`Exit`][EventType::Exit]:[`HeadingAtxText`][Token::HeadingAtxText].
fn on_exit_heading_atx_text(context: &mut CompileContext) {
    let value = context.resume();
    context.push(&*value);
}

/// Handle [`Exit`][EventType::Exit]:[`HeadingSetextText`][Token::HeadingSetextText].
fn on_exit_heading_setext_text(context: &mut CompileContext) {
    let buf = context.resume();
    context.heading_setext_buffer = Some(buf);
    context.slurp_one_line_ending = true;
}

/// Handle [`Exit`][EventType::Exit]:[`HeadingSetextUnderline`][Token::HeadingSetextUnderline].
fn on_exit_heading_setext_underline(context: &mut CompileContext) {
    let text = context
        .heading_setext_buffer
        .take()
        .expect("`atx_opening_sequence_size` must be set in headings");
    let head = codes_from_span(
        context.codes,
        &from_exit_event(context.events, context.index),
    )[0];
    let level: usize = if head == Code::Char('-') { 2 } else { 1 };

    context.line_ending_if_needed();
    context.tag(&*format!("<h{}>", level));
    context.push(&*text);
    context.tag(&*format!("</h{}>", level));
}

/// Handle [`Exit`][EventType::Exit]:{[`HtmlFlow`][Token::HtmlFlow],[`HtmlText`][Token::HtmlText]}.
fn on_exit_html(context: &mut CompileContext) {
    context.ignore_encode = false;
}

/// Handle [`Exit`][EventType::Exit]:{[`HtmlFlowData`][Token::HtmlFlowData],[`HtmlTextData`][Token::HtmlTextData]}.
fn on_exit_html_data(context: &mut CompileContext) {
    let slice = serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    );
    context.push_raw(&*slice);
}

/// Handle [`Exit`][EventType::Exit]:[`Label`][Token::Label].
fn on_exit_label(context: &mut CompileContext) {
    let buf = context.resume();
    let media = context.media_stack.last_mut().unwrap();
    media.label = Some(buf);
}

/// Handle [`Exit`][EventType::Exit]:[`LabelText`][Token::LabelText].
fn on_exit_label_text(context: &mut CompileContext) {
    let media = context.media_stack.last_mut().unwrap();
    media.label_id = Some(serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    ));
}

/// Handle [`Exit`][EventType::Exit]:[`LineEnding`][Token::LineEnding].
fn on_exit_line_ending(context: &mut CompileContext) {
    if context.code_text_inside {
        context.push(" ");
    } else if context.slurp_one_line_ending {
        context.slurp_one_line_ending = false;
    } else {
        context.push_raw(&*serialize(
            context.codes,
            &from_exit_event(context.events, context.index),
            false,
        ));
    }
}

/// Handle [`Exit`][EventType::Exit]:{[`ListOrdered`][Token::ListOrdered],[`ListUnordered`][Token::ListUnordered]}.
fn on_exit_list(context: &mut CompileContext) {
    let tag_name = if context.events[context.index].token_type == Token::ListOrdered {
        "ol"
    } else {
        "ul"
    };
    context.tight_stack.pop();
    context.line_ending();
    context.tag(&*format!("</{}>", tag_name));
}

/// Handle [`Exit`][EventType::Exit]:[`ListItem`][Token::ListItem].
fn on_exit_list_item(context: &mut CompileContext) {
    let tight = context.tight_stack.last().unwrap_or(&false);
    let before_item = skip::opt_back(
        context.events,
        context.index - 1,
        &[
            Token::BlankLineEnding,
            Token::LineEnding,
            Token::SpaceOrTab,
            Token::BlockQuotePrefix,
        ],
    );
    let previous = &context.events[before_item];
    let tight_paragraph = *tight && previous.token_type == Token::Paragraph;
    let empty_item = previous.token_type == Token::ListItemPrefix;

    context.slurp_one_line_ending = false;

    if !tight_paragraph && !empty_item {
        context.line_ending_if_needed();
    }

    context.tag("</li>");
}

/// Handle [`Exit`][EventType::Exit]:[`ListItemValue`][Token::ListItemValue].
fn on_exit_list_item_value(context: &mut CompileContext) {
    let expect_first_item = context.expect_first_item.unwrap();

    if expect_first_item {
        let slice = serialize(
            context.codes,
            &from_exit_event(context.events, context.index),
            false,
        );
        let value = slice.parse::<u32>().ok().unwrap();

        if value != 1 {
            context.tag(" start=\"");
            context.tag(&*value.to_string());
            context.tag("\"");
        }
    }
}

/// Handle [`Exit`][EventType::Exit]:{[`Image`][Token::Image],[`Link`][Token::Link]}.
fn on_exit_media(context: &mut CompileContext) {
    let mut is_in_image = false;
    let mut index = 0;

    // Skip current.
    while index < (context.media_stack.len() - 1) {
        if context.media_stack[index].image {
            is_in_image = true;
            break;
        }
        index += 1;
    }

    context.tags = !is_in_image;

    let media = context.media_stack.pop().unwrap();
    let id = media
        .reference_id
        .or(media.label_id)
        .map(|id| normalize_identifier(&id));
    let label = media.label.unwrap();
    let mut definition: Option<&Definition> = None;

    if let Some(id) = id {
        let mut index = 0;

        while index < context.definitions.len() {
            if context.definitions[index].0 == id {
                definition = Some(&context.definitions[index].1);
                break;
            }

            index += 1;
        }
    }

    let destination = if media.destination.is_some() {
        &media.destination
    } else {
        &definition.unwrap().destination
    };
    let title = if media.destination.is_some() {
        &media.title
    } else {
        &definition.unwrap().title
    };

    let destination = if let Some(destination) = destination {
        destination
    } else {
        ""
    };

    let title = if let Some(title) = title {
        format!(" title=\"{}\"", title)
    } else {
        "".to_string()
    };

    if media.image {
        context.tag(&*format!(
            "<img src=\"{}\" alt=\"",
            sanitize_uri(destination, &context.protocol_src),
        ));
        context.push(&*label);
        context.tag(&*format!("\"{} />", title));
    } else {
        context.tag(&*format!(
            "<a href=\"{}\"{}>",
            sanitize_uri(destination, &context.protocol_href),
            title,
        ));
        context.push(&*label);
        context.tag("</a>");
    };
}

/// Handle [`Exit`][EventType::Exit]:[`Paragraph`][Token::Paragraph].
fn on_exit_paragraph(context: &mut CompileContext) {
    let tight = context.tight_stack.last().unwrap_or(&false);

    if *tight {
        context.slurp_one_line_ending = true;
    } else {
        context.tag("</p>");
    }
}

/// Handle [`Exit`][EventType::Exit]:[`ReferenceString`][Token::ReferenceString].
fn on_exit_reference_string(context: &mut CompileContext) {
    // Drop stuff.
    context.resume();
    let media = context.media_stack.last_mut().unwrap();
    media.reference_id = Some(serialize(
        context.codes,
        &from_exit_event(context.events, context.index),
        false,
    ));
}

/// Handle [`Exit`][EventType::Exit]:[`ResourceDestinationString`][Token::ResourceDestinationString].
fn on_exit_resource_destination_string(context: &mut CompileContext) {
    let buf = context.resume();
    let media = context.media_stack.last_mut().unwrap();
    media.destination = Some(buf);
    context.ignore_encode = false;
}

/// Handle [`Exit`][EventType::Exit]:[`ResourceTitleString`][Token::ResourceTitleString].
fn on_exit_resource_title_string(context: &mut CompileContext) {
    let buf = context.resume();
    let media = context.media_stack.last_mut().unwrap();
    media.title = Some(buf);
}

/// Handle [`Exit`][EventType::Exit]:[`Strong`][Token::Strong].
fn on_exit_strong(context: &mut CompileContext) {
    context.tag("</strong>");
}

/// Handle [`Exit`][EventType::Exit]:[`ThematicBreak`][Token::ThematicBreak].
fn on_exit_thematic_break(context: &mut CompileContext) {
    context.line_ending_if_needed();
    context.tag("<hr />");
}

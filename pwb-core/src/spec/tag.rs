//! Primitive tag types in PlainWeb Markup v0.

/// Primitive tag types in PlainWeb Markup v0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tag {
    Document = 1,
    Head = 2,
    Title = 3,
    View = 4,
    Text = 5,
    Image = 6,
    Button = 7,
    Input = 8,
    Link = 9,
    /// `<script src="...">` / inline `<script>` — bundled by the HTML bundler.
    Script = 10,
    /// `<component name="...">` — a reusable PML component definition.
    Component = 11,
    /// `<import component="...">` — the custom element that inlines a component.
    Import = 12,
    /// `<icon-home>` / `<icon name="home">` — a builtin feather icon.
    Icon = 13,
    /// `<code>` — text rendered with real JS syntax highlighting (strings,
    /// numbers, keywords, comments), unlike HTML's monochrome `<code>`.
    Code = 14,
}

impl Tag {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Tag::Document),
            2 => Some(Tag::Head),
            3 => Some(Tag::Title),
            4 => Some(Tag::View),
            5 => Some(Tag::Text),
            6 => Some(Tag::Image),
            7 => Some(Tag::Button),
            8 => Some(Tag::Input),
            9 => Some(Tag::Link),
            10 => Some(Tag::Script),
            11 => Some(Tag::Component),
            12 => Some(Tag::Import),
            13 => Some(Tag::Icon),
            14 => Some(Tag::Code),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tag::Document => "plain",
            Tag::Head => "head",
            Tag::Title => "title",
            Tag::View => "view",
            Tag::Text => "text",
            Tag::Image => "image",
            Tag::Button => "button",
            Tag::Script => "script",
            Tag::Component => "component",
            Tag::Import => "import",
            Tag::Input => "input",
            Tag::Link => "link",
            Tag::Icon => "icon",
            Tag::Code => "code",
        }
    }
}
impl std::str::FromStr for Tag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "plain" => Ok(Tag::Document),
            "head" => Ok(Tag::Head),
            "title" => Ok(Tag::Title),
            "view" => Ok(Tag::View),
            "text" => Ok(Tag::Text),
            "image" => Ok(Tag::Image),
            "button" => Ok(Tag::Button),
            "input" => Ok(Tag::Input),
            "link" => Ok(Tag::Link),
            "icon" => Ok(Tag::Icon),
            "code" => Ok(Tag::Code),
            _ => Err(()),
        }
    }
}

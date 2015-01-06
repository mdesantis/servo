/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::iter::Iterator;
use std::ascii::AsciiExt;
use url::Url;

use encoding::EncodingRef;

use cssparser::{Parser, decode_stylesheet_bytes,
                QualifiedRuleParser, AtRuleParser, RuleListParser, AtRuleType};
use string_cache::Namespace;
use selectors::{Selector, parse_selector_list};
use parser::ParserContext;
use properties::{PropertyDeclarationBlock, parse_property_declaration_list};
use namespaces::{NamespaceMap, parse_namespace_rule};
use media_queries::{mod, Device, MediaQueryList, parse_media_query_list};
use font_face::{FontFaceRule, Source, parse_font_face_block, iter_font_face_rules_inner};


#[deriving(Clone, PartialEq, Eq, Copy)]
pub enum Origin {
    UserAgent,
    Author,
    User,
}


pub struct Stylesheet {
    /// List of rules in the order they were found (important for
    /// cascading order)
    rules: Vec<CSSRule>,
    pub origin: Origin,
}


pub enum CSSRule {
    Charset(String),
    Namespace(Option<String>, Namespace),
    Style(StyleRule),
    Media(MediaRule),
    FontFace(FontFaceRule),
}

pub struct MediaRule {
    pub media_queries: MediaQueryList,
    pub rules: Vec<CSSRule>,
}


pub struct StyleRule {
    pub selectors: Vec<Selector>,
    pub declarations: PropertyDeclarationBlock,
}


impl Stylesheet {
    pub fn from_bytes_iter<I: Iterator<Vec<u8>>>(
            mut input: I, base_url: Url, protocol_encoding_label: Option<&str>,
            environment_encoding: Option<EncodingRef>, origin: Origin) -> Stylesheet {
        let mut bytes = vec!();
        // TODO: incremental decoding and tokenization/parsing
        for chunk in input {
            bytes.push_all(chunk.as_slice())
        }
        Stylesheet::from_bytes(bytes.as_slice(), base_url, protocol_encoding_label,
                               environment_encoding, origin)
    }

    pub fn from_bytes(bytes: &[u8],
                      base_url: Url,
                      protocol_encoding_label: Option<&str>,
                      environment_encoding: Option<EncodingRef>,
                      origin: Origin)
                      -> Stylesheet {
        // TODO: bytes.as_slice could be bytes.container_as_bytes()
        let (string, _) = decode_stylesheet_bytes(
            bytes.as_slice(), protocol_encoding_label, environment_encoding);
        Stylesheet::from_str(string.as_slice(), base_url, origin)
    }

    pub fn from_str<'i>(css: &'i str, base_url: Url, origin: Origin) -> Stylesheet {
        let rules = Parser::parse_str(css, |input| {
            let mut context = ParserContext {
                stylesheet_origin: origin,
                base_url: &base_url,
                namespaces: NamespaceMap::new()
            };
            let parser = MainRuleParser {
                context: &mut context,
                state: State::Start,
            };
            RuleListParser::new_for_stylesheet(input, parser)
            .filter_map(|result| result.ok())
            .collect()
        });
        Stylesheet {
            origin: origin,
            rules: rules,
        }
    }
}


fn parse_nested_rules(context: &mut ParserContext, input: &mut Parser) -> Vec<CSSRule> {
    let parser = MainRuleParser {
        context: context,
        state: State::Body,
    };
    RuleListParser::new_for_nested_rule(input, parser)
    .filter_map(|result| result.ok())
    .collect()
}


struct MainRuleParser<'a, 'b: 'a> {
    context: &'a mut ParserContext<'b>,
    state: State,
}


#[deriving(Eq, PartialEq, Ord, PartialOrd)]
enum State {
    Start = 1,
    Imports = 2,
    Namespaces = 3,
    Body = 4,
}


enum AtRulePrelude {
    FontFace,
    Media(MediaQueryList),
}


impl<'a, 'b> AtRuleParser<AtRulePrelude, CSSRule> for MainRuleParser<'a, 'b> {
    fn parse_prelude(&mut self, name: &str, input: &mut Parser)
                     -> Result<AtRuleType<AtRulePrelude, CSSRule>, ()> {
        match_ignore_ascii_case! { name:
            "charset" => {
                if self.state <= State::Start {
                    // Valid @charset rules are just ignored
                    self.state = State::Imports;
                    let charset = try!(input.expect_string()).into_owned();
                    return Ok(AtRuleType::WithoutBlock(CSSRule::Charset(charset)))
                } else {
                    return Err(())  // "@charset must be the first rule"
                }
            },
            "import" => {
                if self.state <= State::Imports {
                    self.state = State::Imports;
                    // TODO: support @import
                    return Err(())  // "@import is not supported yet"
                } else {
                    return Err(())  // "@import must be before any rule but @charset"
                }
            },
            "namespace" => {
                if self.state <= State::Namespaces {
                    self.state = State::Namespaces;
                    let (prefix, namespace) = try!(parse_namespace_rule(self.context, input));
                    return Ok(AtRuleType::WithoutBlock(CSSRule::Namespace(prefix, namespace)))
                } else {
                    return Err(())  // "@namespace must be before any rule but @charset and @import"
                }
            }
            _ => {}
        }

        self.state = State::Body;

        match_ignore_ascii_case! { name:
            "media" => {
                let media_queries = parse_media_query_list(input);
                Ok(AtRuleType::WithBlock(AtRulePrelude::Media(media_queries)))
            },
            "font-face" => {
                try!(input.expect_exhausted());
                Ok(AtRuleType::WithBlock(AtRulePrelude::FontFace))
            }
            _ => Err(())
        }
    }

    fn parse_block(&mut self, prelude: AtRulePrelude, input: &mut Parser) -> Result<CSSRule, ()> {
        match prelude {
            AtRulePrelude::FontFace => {
                parse_font_face_block(self.context, input).map(CSSRule::FontFace)
            }
            AtRulePrelude::Media(media_queries) => {
                Ok(CSSRule::Media(MediaRule {
                    media_queries: media_queries,
                    rules: parse_nested_rules(self.context, input),
                }))
            }
        }
    }
}


impl<'a, 'b> QualifiedRuleParser<Vec<Selector>, CSSRule> for MainRuleParser<'a, 'b> {
    fn parse_prelude(&mut self, input: &mut Parser) -> Result<Vec<Selector>, ()> {
        self.state = State::Body;
        parse_selector_list(self.context, input)
    }

    fn parse_block(&mut self, prelude: Vec<Selector>, input: &mut Parser) -> Result<CSSRule, ()> {
        Ok(CSSRule::Style(StyleRule {
            selectors: prelude,
            declarations: parse_property_declaration_list(self.context, input)
        }))
    }
}


pub fn iter_style_rules<'a>(rules: &[CSSRule], device: &media_queries::Device,
                            callback: |&StyleRule|) {
    for rule in rules.iter() {
        match *rule {
            CSSRule::Style(ref rule) => callback(rule),
            CSSRule::Media(ref rule) => if rule.media_queries.evaluate(device) {
                iter_style_rules(rule.rules.as_slice(), device, |s| callback(s))
            },
            CSSRule::FontFace(..) |
            CSSRule::Charset(..) |
            CSSRule::Namespace(..) => {}
        }
    }
}

pub fn iter_stylesheet_media_rules(stylesheet: &Stylesheet, callback: |&MediaRule|) {
    for rule in stylesheet.rules.iter() {
        match *rule {
            CSSRule::Media(ref rule) => callback(rule),
            CSSRule::Style(..) |
            CSSRule::FontFace(..) |
            CSSRule::Charset(..) |
            CSSRule::Namespace(..) => {}
        }
    }
}

#[inline]
pub fn iter_stylesheet_style_rules(stylesheet: &Stylesheet, device: &media_queries::Device,
                                   callback: |&StyleRule|) {
    iter_style_rules(stylesheet.rules.as_slice(), device, callback)
}


#[inline]
pub fn iter_font_face_rules(stylesheet: &Stylesheet, device: &Device,
                            callback: |family: &str, source: &Source|) {
    iter_font_face_rules_inner(stylesheet.rules.as_slice(), device, callback)
}

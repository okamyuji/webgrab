//! 要素の入れ子の深さの判定。htmdの変換は入れ子を再帰でたどってスタックを溢れさせ、
//! dom_smoothieの抽出は深さに対して計算量が急増するため、どちらの前にもこの判定を通す。
//!
//! 深さは、変換や抽出と同じHTML5パーサー（html5ever）が作る木で数える。字句で数えると
//! `<div/>`や対応の無い閉じタグのように開いたまま残る形を取り逃がし、逆にsvgの`<path/>`や
//! 閉じタグを省いた`<li>`を多めに数えてしまう。木の構築は深さに対して二乗時間なので、
//! 入力を少しずつ渡し、上限を超えた時点で解析を打ち切る。

use dom_query::{Document, NodeId};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, ParseOpts, QualName, parse_document};
use std::borrow::Cow;
use std::cell::{Cell, Ref};

/// 要素の入れ子の深さの上限（htmlとbodyの2段を含む）。実ページはまず数百段を超えない。
pub(crate) const MAX_NESTING_DEPTH: usize = 1000;

/// 一度にパーサーへ渡す入力の大きさ。1回分の入力で増える深さは
/// この大きさの3分の1程度に収まり、打ち切るまでの二乗の計算を小さく保つ。
const CHUNK_BYTES: usize = 1 << 13;

/// 入れ子の最大深さが`limit`を超えるかを返す。
pub(crate) fn exceeds_nesting_depth(html: &str, limit: usize) -> bool {
    let mut parser = parse_document(DepthProbe::default(), parse_opts());
    for chunk in utf8_chunks(html, CHUNK_BYTES) {
        parser.process(StrTendril::from_slice(chunk));
        if parser.tokenizer.sink.sink.newest_depth_exceeds(limit) {
            return true;
        }
    }
    tree_depth_exceeds(&parser.finish().doc, limit)
}

/// dom_query（dom_smoothieの抽出が使う）と同じ解析オプション。scriptingを無効にすると
/// `<noscript>`の中身も要素になり、htmd（scripting有効）より深さを多めに数える側になる。
fn parse_opts() -> ParseOpts {
    ParseOpts {
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// `s`を、UTF-8の文字の境界で`max`バイト以下の断片に分ける。
fn utf8_chunks(s: &str, max: usize) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = max.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            // 上限が1文字より小さいときも、空の断片を出し続けないよう1文字は進める。
            end = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        let (head, tail) = rest.split_at(end);
        rest = tail;
        Some(head)
    })
}

/// 完成した木で、要素の入れ子の深さを再帰せずに数える。
fn tree_depth_exceeds(doc: &Document, limit: usize) -> bool {
    let mut stack: Vec<_> = doc
        .root()
        .element_children()
        .into_iter()
        .map(|n| (n, 1))
        .collect();
    while let Some((node, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        let mut child = node.first_element_child();
        while let Some(c) = child {
            child = c.next_element_sibling();
            stack.push((c, depth + 1));
        }
    }
    false
}

/// dom_queryの木を作りながら、最後に作った要素を覚えておくTreeSink。
/// 解析の途中でその要素の深さを調べ、深い入力を早く打ち切るために使う。
#[derive(Default)]
struct DepthProbe {
    doc: Document,
    newest: Cell<Option<NodeId>>,
}

impl DepthProbe {
    /// 最後に作った要素の深さが`limit`を超えるか。祖先は`limit + 1`段までしか数えない。
    /// 祖先には文書ノードも含まれるので、数えた段数がhtmlを1とする深さに一致する。
    fn newest_depth_exceeds(&self, limit: usize) -> bool {
        self.newest.get().is_some_and(|id| {
            self.doc
                .tree
                .ancestor_ids_of_it(&id, Some(limit + 1))
                .count()
                > limit
        })
    }
}

impl TreeSink for DepthProbe {
    type Handle = NodeId;
    type Output = Self;
    type ElemName<'a> = Ref<'a, QualName>;

    fn finish(self) -> Self {
        self
    }
    fn parse_error(&self, msg: Cow<'static, str>) {
        self.doc.parse_error(msg);
    }
    fn get_document(&self) -> NodeId {
        self.doc.get_document()
    }
    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        self.doc.elem_name(target)
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        let id = self.doc.create_element(name, attrs, flags);
        self.newest.set(Some(id));
        id
    }
    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.doc.create_comment(text)
    }
    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.doc.create_pi(target, data)
    }
    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        self.doc.append(parent, child);
    }
    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        self.doc
            .append_based_on_parent_node(element, prev_element, child);
    }
    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        self.doc
            .append_doctype_to_document(name, public_id, system_id);
    }
    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.doc.get_template_contents(target)
    }
    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        self.doc.same_node(x, y)
    }
    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.doc.set_quirks_mode(mode);
    }
    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        self.doc.append_before_sibling(sibling, new_node);
    }
    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        self.doc.add_attrs_if_missing(target, attrs);
    }
    fn remove_from_parent(&self, target: &NodeId) {
        self.doc.remove_from_parent(target);
    }
    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.doc.reparent_children(node, new_parent);
    }
    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        self.doc.is_mathml_annotation_xml_integration_point(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_chunks_split_on_char_boundaries_and_keep_all_bytes() {
        let s = "aあいb".repeat(5);
        for max in [2, 4, 5] {
            let chunks: Vec<&str> = utf8_chunks(&s, max).collect();
            assert!(chunks.iter().all(|c| !c.is_empty()), "max={max}");
            assert!(chunks.iter().all(|c| c.len() <= max.max(3)), "max={max}");
            assert_eq!(chunks.concat(), s, "max={max}");
        }
        assert_eq!(utf8_chunks("aあ", 2).collect::<Vec<_>>(), ["a", "あ"]);
        assert_eq!(utf8_chunks("あい", 1).collect::<Vec<_>>(), ["あ", "い"]);
        assert_eq!(utf8_chunks("", 4).count(), 0);
    }

    /// 包んだTreeSinkで、dom_queryと同じ木ができること。断片に分けて渡す経路も通す。
    #[test]
    fn probe_builds_the_same_tree_as_dom_query() {
        let cases = [
            "<!DOCTYPE html><p>standards</p>",
            "<p>quirks without doctype</p>",
            "<table>text<tr><td>cell</td></tr><div>fostered</div></table>",
            "<b><p>one</b>two</p>",
            "<a href=x><div>adopted</a>after</div>",
            "<html><body><html lang=en data-x=1><body class=c>",
            "<template><p>inside</p></template><p>outside</p>",
            r#"<math><annotation-xml encoding="text/html"><div>html</div></annotation-xml></math>"#,
            "<noscript><p>shown without scripting</p></noscript>",
            "<math><annotation-xml><div>breaks out</div></annotation-xml></math>",
            "<p><frameset><frame></frameset>",
            "<form><input name=a></form><p><input form=f></p>",
            "<!-- comment --><p>x</p></i></span><p>",
        ];
        for html in cases {
            let want = Document::from(html);
            let mut parser = parse_document(DepthProbe::default(), parse_opts());
            for chunk in utf8_chunks(html, 3) {
                parser.process(StrTendril::from_slice(chunk));
            }
            let got = parser.finish().doc;
            assert_eq!(got.html(), want.html(), "{html}");
            assert_eq!(got.quirks_mode.get(), want.quirks_mode.get(), "{html}");
            // 解析エラーの数はテキストの区切り方で変わるため、一度に渡して比べる。
            let whole = parse_document(DepthProbe::default(), parse_opts())
                .one(html)
                .doc;
            assert_eq!(
                whole.errors.borrow().len(),
                want.errors.borrow().len(),
                "{html}"
            );
        }
    }

    #[test]
    fn depth_checked_at_a_chunk_boundary_matches_the_final_tree() {
        // 上限ちょうどの入れ子を、断片の境界をまたいで渡しても誤って打ち切らない。
        let n = MAX_NESTING_DEPTH - 2;
        let at_limit = format!("{}x{}", "<div>".repeat(n), "</div>".repeat(n));
        assert!(at_limit.len() > CHUNK_BYTES);
        assert!(!exceeds_nesting_depth(&at_limit, MAX_NESTING_DEPTH));
        let over = format!("{}x", "<div>".repeat(n + 1));
        assert!(exceeds_nesting_depth(&over, MAX_NESTING_DEPTH));
    }

    #[test]
    fn deep_subtree_closed_within_one_chunk_is_caught_by_the_final_count() {
        // 1回分の入力の中で深くなって閉じる形は、途中の確認では見えず、完成した木で数える。
        let n = 50;
        let html = format!("{}x{}<p>after</p>", "<b>".repeat(n), "</b>".repeat(n));
        assert!(html.len() < CHUNK_BYTES);
        assert!(exceeds_nesting_depth(&html, 40));
        assert!(!exceeds_nesting_depth(&html, n + 2));
    }

    #[test]
    fn nesting_depth_counts_html_and_body() {
        assert!(!exceeds_nesting_depth("abc", 2));
        assert!(exceeds_nesting_depth("abc", 1));
        assert!(!exceeds_nesting_depth("<!DOCTYPE html><?xml?>a < b <1", 2));
    }

    #[test]
    fn nesting_depth_follows_open_and_close_tags() {
        assert!(!exceeds_nesting_depth(
            r#"<p class="a"></p><p id="b"></p>"#,
            3
        ));
        assert!(exceeds_nesting_depth("<div><div></div></div>", 3));
        assert!(!exceeds_nesting_depth("<div><div></div></div>", 4));
    }

    #[test]
    fn nesting_depth_does_not_count_void_elements() {
        assert!(!exceeds_nesting_depth(
            &r#"<br><BR><img src="a"><hr/>"#.repeat(50),
            3
        ));
    }

    #[test]
    fn nesting_depth_does_not_count_implicitly_closed_list_items() {
        let list = format!("<ul>{}</ul>", "<li>item".repeat(2000));
        assert!(!exceeds_nesting_depth(&list, 4));
    }

    #[test]
    fn nesting_depth_does_not_count_self_closed_foreign_elements() {
        // svgの`<path/>`は実際に閉じるので、何個並んでも深くならない。
        let icons = r#"<svg><path d="M0"/></svg>"#.repeat(2000);
        assert!(!exceeds_nesting_depth(&icons, 5));
    }

    #[test]
    fn deep_forms_are_rejected_before_parsing_the_whole_input() {
        // 木の構築は深さに対して二乗時間なので、上限を超えた時点で解析を打ち切る。
        // `<table>`の後の`<div>`はtableの前へ差し込まれ（foster parenting）、その中で深くなる。
        for (head, unit) in [
            ("", "<div>"),
            ("", "<div/>"),
            ("", "<div></i>"),
            ("", r#"<div a="/>">"#),
            ("<table>", "<div>"),
        ] {
            let html = format!("{head}{}", unit.repeat(200_000));
            let start = std::time::Instant::now();
            assert!(
                exceeds_nesting_depth(&html, MAX_NESTING_DEPTH),
                "{head}{unit}"
            );
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "{head}{unit}: {:?}",
                start.elapsed()
            );
        }
    }

    #[test]
    fn nesting_depth_counts_forms_the_parser_keeps_open() {
        // HTML5では非void要素の`/>`、対応の無い閉じタグ、引用符内の`/>`は要素を閉じない。
        for unit in ["<div/>", "<span/>", "<div></i>", r#"<div a="/>">"#] {
            assert!(exceeds_nesting_depth(&unit.repeat(50), 40), "{unit}");
        }
    }
}

use comrak::{parse_document, Arena, Options, nodes::{AstNode, NodeValue}};

fn main() {
    let content = "This is a test of the **Kitty** graphics protocol.";
    let arena = Arena::new();
    let root = parse_document(&arena, content, &Options::default());
    
    walk_node(root, 0);
}

fn walk_node<'a>(node: &'a AstNode<'a>, indent: usize) {
    let value = &node.data.borrow().value;
    println!("{:indent$}{:?}", "", value, indent = indent);
    for child in node.children() {
        walk_node(child, indent + 2);
    }
}

# Galley

A lightweight viewer for **GitHub-flavoured Markdown**, with *emphasis*, `inline code`,
~~strikethrough~~ and [links](https://github.com/odclcslkrpb5k/galley).

> [!TIP]
> Saving the file reloads it in place: the view stays where it was.

## Code

```rust
/// Counts the words in `text`.
fn words(text: &str) -> usize {
    text.split_whitespace().count() // 42 for this file, more or less
}
```

```console
$ galley --theme=dark README.md
```

## Tables and tasks

| Goal | Measure | Status |
|:---|:---:|---:|
| First frame | under 200 ms | met |
| Scrolling | no dropped frames | met |
| GitHub fidelity | 21 READMEs | met |

- [x] Light and dark
- [x] Syntax highlighting
- [ ] Find in page

<details open>
<summary>More, folded away</summary>

Footnotes[^1], and `<details>` blocks like this one.

</details>

[^1]: Numbered as GitHub numbers them.

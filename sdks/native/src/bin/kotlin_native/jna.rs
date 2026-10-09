//! Adapt UniFFI's JNA implementation to the generated common declarations.
use anyhow::{Context, Result};
use camino::Utf8Path;
use uniffi_bindgen::ComponentInterface;

pub(super) fn adapt(path: &Utf8Path, ci: &ComponentInterface) -> Result<()> {
    let mut source = std::fs::read_to_string(path)?;
    source = source
        .replace("NoHandle", "NoPointer")
        .replace("noHandle", "noPointer");
    source = source.replace(
        "class InternalException(message: String) : kotlin.Exception(message)",
        "",
    );
    remove(&mut source, "interface Disposable")?;
    let start = source
        .find("inline fun <T : Disposable?, R>")
        .context("missing JNA use helper")?;
    let end = source[start..]
        .find("object UniffiWithHandle")
        .context("missing handle marker")?
        + start;
    source.replace_range(start..end, "");
    source = source.replace("object NoPointer", "");
    for rec in ci.record_definitions() {
        let name = super::render::public_name(ci, rec.name());
        remove(&mut source, &format!("data class {name} "))?;
    }
    for e in ci.enum_definitions() {
        let name = super::render::public_name(ci, e.name());
        remove(&mut source, &format!("sealed class {name}"))?;
        if ci.is_name_used_as_error(e.name()) {
            source = source
                .replace(&format!("{name}.ErrorHandler"), &format!("{name}Handler"))
                .replace(
                    &format!("uniffiRustCallWithError({name})"),
                    &format!("uniffiRustCallWithError({name}Handler)"),
                );
            source.push_str(&format!("\nprivate object {name}Handler : UniffiRustCallStatusErrorHandler<{name}> {{\n override fun lift(error_buf: RustBuffer.ByValue): {name} = FfiConverterType{}.lift(error_buf)\n}}\n", e.name()));
        }
    }
    for obj in ci.object_definitions() {
        let (interface, implementation) = super::render::object_names(ci, obj);
        remove(&mut source, &format!("public interface {interface} "))?;
        let needle = format!("open class {implementation}:");
        let start = source.find(&needle).context("missing JNA object")?;
        let end = block_end(&source, start)?;
        let body = &source[start..end];
        let mut actual = body
            .replace(&needle, &format!("actual open class {implementation}:"))
            .replace("override fun ", "actual override fun ")
            .replace("override suspend fun ", "actual override suspend fun ")
            .replace("companion object", "actual companion object");
        // The nested disposable and cleaner implementations have no common declarations.
        actual = actual
            .replace("actual override fun run()", "override fun run()")
            .replace(
                "actual override fun destroy() {\n            if (handle",
                "override fun destroy() {\n            if (handle",
            );
        actual = actual.replace(
            "constructor(noPointer: NoPointer)",
            "actual constructor(noPointer: NoPointer)",
        );
        if obj
            .primary_constructor()
            .is_some_and(|cons| !cons.is_async())
        {
            actual = actual.replace("constructor()", "actual constructor()");
        }
        // Alternate constructors are the only public functions in the companion.
        if let Some(at) = actual.find("actual companion object") {
            let (before, after) = actual.split_at(at);
            actual = format!(
                "{}{}",
                before,
                after
                    .replace("fun `", "actual fun `")
                    .replace("suspend actual fun", "actual suspend fun")
            );
        }
        source.replace_range(start..end, &actual);
    }
    for func in ci.function_definitions() {
        source = source.replace(
            &format!("fun `{}`", super::render::function_name(func.name())),
            &format!("actual fun `{}`", super::render::function_name(func.name())),
        );
    }
    std::fs::write(path, async_throws(&source)?)?;
    Ok(())
}

fn block_end(source: &str, start: usize) -> Result<usize> {
    let open = source[start..]
        .find('{')
        .context("missing declaration body")?
        + start;
    let mut depth = 0;
    for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(open + offset + 1);
                }
            }
            _ => {}
        }
    }
    anyhow::bail!("unterminated JNA declaration")
}
fn remove(source: &mut String, needle: &str) -> Result<()> {
    let start = source
        .find(needle)
        .with_context(|| format!("missing JNA declaration {needle}"))?;
    let end = block_end(source, start)?;
    // Include the visibility keyword when present.
    let start = if source[..start].ends_with("public ") {
        start - 7
    } else {
        start
    };
    source.replace_range(start..end, "");
    Ok(())
}

// Common suspend declarations include cancellation in their throws contract.
fn async_throws(source: &str) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in source.lines() {
        if line.contains("actual ") && line.contains("suspend fun `") {
            let annotation = (0..lines.len())
                .rev()
                .take_while(|&index| {
                    let previous = lines[index].trim();
                    previous.is_empty() || previous.starts_with('@')
                })
                .find(|&index| lines[index].trim().starts_with("@Throws("));
            if let Some(index) = annotation {
                let end = lines[index]
                    .rfind(')')
                    .context("missing throws terminator")?;
                lines[index].insert_str(
                    end,
                    ", kotlin.coroutines.cancellation.CancellationException::class",
                );
            } else {
                let indent = &line[..line.len() - line.trim_start().len()];
                lines.push(format!(
                    "{indent}@Throws(kotlin.coroutines.cancellation.CancellationException::class)"
                ));
            }
        }
        lines.push(line.to_owned());
    }
    Ok(lines.join("\n") + "\n")
}

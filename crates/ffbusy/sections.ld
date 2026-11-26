/* Keep each front end's code out of the daemon's hot path.
 *
 * ff, fflocated and ff-gui share one image in ffbusy. Left to the default
 * layout, the linker interleaves input sections, so `ff_gui`/`rswidgets` or
 * `ff_cli` functions can land between daemon functions and be pulled in by
 * the readahead around a text fault. Collecting them into sections placed
 * after `.text` keeps the daemon's executed pages contiguous at the front of
 * the image; their pages are then faulted in only when that front end runs,
 * since text is demand-paged.
 *
 * Section names carry each function's symbol, and the v0 mangling encodes the
 * crate name as `_<len><crate>` (`..._6ff_gui`, `..._14rustxwidgets`,
 * `..._6ff_cli`), which the patterns below match. `INSERT AFTER .text`
 * extends the default script rather than replacing it, so every other section
 * keeps its placement.
 *
 * Measured on x86-64 with fat LTO (this host, 128 KB readahead). The GUI
 * section claims 352,638 B and the CLI section 201,031 B, exactly what `.text`
 * lost -- and in an unstripped link every widget-named text symbol (295) and
 * every CLI-named one (199) resolves inside its own section, none outside.
 *
 * A daemon started from the image executes only `.text`: a fresh instance
 * leaves 80 of the GUI's 87 pages and 49 of the CLI's 50 out of memory. The
 * handful that are resident sit on the section boundaries and vary run to run
 * with page-cache state (7 and 1 here, 11 and 13 with a warmer cache); repeated
 * samples do not grow them. `ff watch` faults all 50 CLI pages contiguously.
 * Unordered, that code would sit among the daemon's hot code instead.
 *
 * The CLI needs the same treatment for the same reason: a daemon never runs
 * `ff`'s query/expression/format code, and that code is ~the whole CLI. It
 * goes after the widget section so the daemon's own text stays adjacent to
 * the front of the image.
 *
 * Monomorphizations left behind in `.text` are std/core generics over each
 * front end's types (`drop_glue::<Box<...Canvas>>`, stable sort over
 * `CellValue`): their section names carry no crate name, so no section-name
 * pattern can claim them. They are reachable only from the front end that
 * owns the type, so the daemon still never executes them.
 */
SECTIONS
{
  .text.gui :
  {
    *(.text.*ff_gui*)
    *(.text.*rswidgets*)
    *(.text.*gtk_dynamic_loader*)
  }
  .text.cli :
  {
    *(.text.*ff_cli*)
  }
}
INSERT AFTER .text;

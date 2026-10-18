# WarraqCraft user guide

## Start a document

- **New file** creates an untitled, unsaved buffer. It is not written to
  disk until you choose **Save As** or provide a path through a command.
- **New from template** opens the gallery. Search, filter by category or
  language, preview the source, then choose a filename/location.
- **Open** loads an existing `.tex`, `.bib` or related text source.

A dirty buffer is marked in the title/status area. Autosave keeps the
latest text, last path and cursor position in preferences so the next
launch can restore the interrupted state.

## Compile and navigate

1. Press **Compile** / **Recompile**.
2. The log pane lists errors, warnings and file paths when available.
3. Click a diagnostic to open the right file and line.
4. Ctrl+click source to jump to the matching PDF location; click the PDF
   to jump back to source when SyncTeX is available.

If no engine is found, install a system XeLaTeX/LuaLaTeX/pdfLaTeX or run
`scripts/fetch-tectonic.ps1` to place the optional Tectonic binary in
`bin/`.

## Edit faster

- Use the toolbar for bold, italic, underline, sections, inline math and
  common environments.
- Toggle the symbol palette for categorized Greek letters, arrows,
  operators and relations.
- Start `\ref{` or `\cite{` for project-aware completion.
- Use the outline to navigate the whole `\input`/`\include` project.
- Right-click spell-check suggestions when dictionaries are installed.

## Project history

Snapshots live under the project-local `.warraqcraft/` directory. Create a
snapshot before a large edit and restore through the History window.
Snapshots are local project history; they are separate from preference
autosave recovery.

## Languages and RTL

The UI supports English, Arabic, Persian, Urdu, French, Italian, German,
Spanish, Portuguese, Turkish, Latin, Russian, Mandarin Chinese, Japanese,
Korean and Hindi. English is the fallback for missing text.

For Arabic-script documents use a Unicode-aware engine such as XeLaTeX or
LuaLaTeX and the bundled Arabic starter templates. The UI display layer
handles bidirectional text, while the editor keeps the underlying `.tex`
source in normal logical order.

## AI assist

Open **AI assist**, choose a provider, then either run local setup or
enter a bring-your-own endpoint:

- **Ollama** fills the local endpoint/model and can start the service and
  pull the selected model. If the CLI is missing, the explicit install
  action uses the platform package manager where supported.
- **OpenAI**, **OpenRouter** or **Custom** only need endpoint/model plus
  a token. The token stays in memory for the session and is never saved.
- **Test connection** sends only a `/v1/models` handshake. **Ask** sends
  a bounded copy of the current document as context.

Agents can use the same provider path headlessly:

```text
warraqcraft-cli cmd ai.providers
warraqcraft-cli cmd ai.probe --params '{"provider":"ollama"}'
warraqcraft-cli cmd ai.ask --params '{"prompt":"Summarise the errors"}'
```

## Automation

```text
warraqcraft-cli commands
warraqcraft-cli cmd document.compile --file main.tex
warraqcraft-cli cmd document.new
warraqcraft-cli mcp
```

MCP and the GUI control channel are opt-in integrations for agents. The
control channel starts only when the GUI is launched with `--control`.

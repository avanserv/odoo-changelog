# odoo-changelog

Read `CONTRIBUTING.md` before changing anything: it holds the layering, determinism, testing,
commit and documentation rules, and they bind agents as they bind people.

## Documentation

- Keep `README.md` under 50 lines. Add to it only a link to a new page, or a change to the
  install and run instructions.
- Put every other explanation in a page under `docs/`: a command's behaviour in
  `docs/<command>.md`, an extractor's symbols and attributes in `docs/extractors/<name>.md`.
- Update the page in the same commit as a change to what a user of the binary or its database
  sees: a symbol kind, a qualname, an attribute, a command's flags or failure cases.
- Link a new page from `README.md`; link a new extractor page from `docs/extract.md` too.
- Use the `docs` skill for any of the above when it is installed.

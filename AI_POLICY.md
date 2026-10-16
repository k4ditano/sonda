# AI policy

pleamar is built with the help of AI, and it's meant to be used with AI: the
installer teaches your agent the language, so you can ask it for a bar. So,
contributions made with AI are welcome, in code, docs and scenes alike.
What matters is not *who* typed it, but that it's good, and that a person
stands behind it.

This applies to Sonda as it does to [pleamar](https://github.com/k4ditano/pleamar),
[pleamar-wm](https://github.com/k4ditano/pleamar-wm) and
[Marea](https://github.com/k4ditano/marea-plm).

## If AI helped with your contribution

- **Say so.** One line in the pull request is enough, e.g. "Written with
  Claude Code, reviewed and tested by me". No need to detail prompts.
- **You're responsible for it.** You've read every line, you understand what
  it does and why, and you can answer questions about it in the review. "The
  AI wrote it" is not an answer to a bug.
- **Test it.** `cargo build` and `cargo test` pass, `pleamar --check
  ui/sonda.plm` is happy if you touched the window, and for anything visual
  you've seen it running: a screenshot or a short clip in the PR helps a lot.
  The headless mode (`pleamar-wm headless`) lets you do it without taking over
  your screen, and a test home (`XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
  `XDG_CACHE_HOME` and `SONDA_SOCKET` pointed elsewhere) keeps your own files
  and index out of it.
- **Keep it focused.** One change per pull request. A large generated PR that
  touches everything is hard to review and will likely be asked to be split.
- **Follow the house style.** Code, comments and commits in English; comments
  that say *why*, the way the surrounding code does. AI tends to add noise
  (restating the code, generic comments): please trim it.
- **Don't invent.** Language words, services and APIs must exist:
  `pleamar --grammar` lists them. Made-up features in docs or examples will be
  rejected.

## Issues and discussions

Using AI to write a bug report or translate your message is fine. Please make
sure the steps to reproduce are real and that you've actually seen the
problem.

## Licensing

By contributing you confirm you have the right to submit the work under the
project's MIT license, whatever tools you used to write it. Don't
paste code from sources with incompatible licenses, AI-suggested or not.

## The maintainer

Much of Sonda, like pleamar, pleamar-wm and Marea, has been written with AI
assistance (Claude), designed, reviewed, tested and used every day by its
maintainer.

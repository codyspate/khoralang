# khoralang.com

This directory is the source tree for the public Khora website and documentation at `khoralang.com`.

It is deliberately separate from the repository's existing `docs/` directory.

- `docs/` contains design records, implementation notes, the compiler/runtime roadmap, errata, and other material written for people developing Khora itself.
- `website/` contains material intended for people evaluating, learning, using, deploying, or operating Khora.
- `website/content/docs/` is the canonical source for the public language documentation.

Do not move internal design records into this tree merely to make them public. Public documentation should teach the language from a user's mental model and may link to an internal design record when deeper rationale is useful.

The website may eventually also contain downloads, releases, benchmarks, ecosystem/package pages, and project news. Those belong under `website/`, but not necessarily under `website/content/docs/`.

## Documentation families

The public documentation is organized around these stable families:

- `getting-started/` — installation and first project
- `reference/` — the language: every construct, one page per topic, ordered so
  a straight read works. It absorbed the former `guide/`, whose pages redirect.
- `stdlib/` — the standard library: prose pages for the modules that need one,
  and generated API documentation beneath them
- `cookbook/` — production patterns and examples
- `migration/` — guides for developers coming from other ecosystems
- `deployment/` — supported targets and deployment workflows
- `limitations/` — current limitations and stability status
- `project/` — release readiness, compatibility policy, governance, security, and contribution information

## Versioning

The public site must make the documentation version explicit once public releases exist. Documentation for a tagged compiler release must remain available after a newer compiler ships.

The intended public shape is:

- `khoralang.com/docs/` — current stable release
- `khoralang.com/docs/<version>/` — pinned release documentation
- `khoralang.com/docs/next/` — development documentation, clearly marked unstable

The exact site generator is intentionally not fixed here. The content layout and URL contract should survive a change in frontend framework.

## Deployment

The site is intended to be deployed through Cloudflare. The repository should remain the source of truth: a website deployment is built from a known Git revision, and a release deployment should be tied to the same tag that produced the compiler artifacts.

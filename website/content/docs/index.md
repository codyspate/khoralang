---
title: Khora
description: A native language for reliable concurrent applications.
template: splash
hero:
  tagline: Native code. Typed failure. Capabilities. Structured concurrency. Direct-style programs.
  actions:
    - text: Get started
      link: /docs/getting-started/
      icon: right-arrow
      variant: primary
    - text: Read the guide
      link: /docs/guide/
      icon: open-book
---

<div class="khora-home">
  <section class="khora-home-intro">
    <div class="khora-home-copy">
      <p class="khora-eyebrow">A native language for reliable systems</p>
      <h2>Make the important parts of a program visible.</h2>
      <p class="khora-lede">Khora is a statically typed, native-compiled language built around explicit failure, explicit authority, structured concurrency, and ordinary direct-style code.</p>
      <div class="khora-inline-links">
        <a href="/docs/guide/effects-and-capabilities/">Effects &amp; capabilities <span aria-hidden="true">→</span></a>
        <a href="/docs/guide/errors-and-raises/">Typed failure <span aria-hidden="true">→</span></a>
      </div>
    </div>

    <div class="khora-code-window" aria-label="Khora function signature example">
      <div class="khora-code-window-bar">
        <span></span><span></span><span></span>
        <span class="khora-code-filename">service.kh</span>
      </div>
      <pre><code><span class="khora-code-keyword">export fn</span> <span class="khora-code-fn">load_user</span>(id: Id) -&gt; User
  <span class="khora-code-keyword">with</span> { db: Db }
  <span class="khora-code-keyword">raises</span> DbError</code></pre>
      <div class="khora-code-legend">
        <span><i class="khora-dot khora-dot-capability"></i><code>with</code> says what the function may access</span>
        <span><i class="khora-dot khora-dot-error"></i><code>raises</code> says how it may fail</span>
      </div>
    </div>
  </section>

  <section class="khora-feature-grid" aria-label="Khora language features">
    <a class="khora-feature-card" href="/docs/guide/effects-and-capabilities/">
      <span class="khora-feature-icon" aria-hidden="true">↯</span>
      <h3>Capabilities, not globals</h3>
      <p>External authority is part of the function type. Dependencies stay visible without threading services through every call.</p>
      <span class="khora-card-link">Explore capabilities →</span>
    </a>

    <a class="khora-feature-card" href="/docs/guide/errors-and-raises/">
      <span class="khora-feature-icon" aria-hidden="true">!</span>
      <h3>Typed failure in direct style</h3>
      <p>Recoverable failures live in <code>raises</code> rows. Handle them explicitly without wrapping the whole program in a monad.</p>
      <span class="khora-card-link">Explore typed failure →</span>
    </a>

    <a class="khora-feature-card" href="/docs/guide/fibers-and-nurseries/">
      <span class="khora-feature-icon" aria-hidden="true">⋈</span>
      <h3>Structured concurrency</h3>
      <p>Fibers belong to lexical lifetimes. Cancellation, cleanup, and joining are designed into the shape of concurrent code.</p>
      <span class="khora-card-link">Explore concurrency →</span>
    </a>

    <a class="khora-feature-card" href="/docs/reference/memory-and-resources/">
      <span class="khora-feature-icon" aria-hidden="true">◇</span>
      <h3>Native without ownership ceremony</h3>
      <p>Khora compiles to native code and derives the ownership plan from ordinary functional programs instead of asking you to write one.</p>
      <span class="khora-card-link">Explore the model →</span>
    </a>
  </section>

  <section class="khora-home-split">
    <div class="khora-home-panel khora-home-panel-accent">
      <p class="khora-eyebrow">Built for application code</p>
      <h2>Functional ideas without functional ceremony.</h2>
      <p>Khora keeps the parts that make large systems easier to reason about—immutable values, algebraic data types, typed errors, effect polymorphism, and explicit capabilities—while keeping the surface syntax direct.</p>
      <div class="khora-pipeline">
        <code>orders</code>
        <span>|&gt;</span>
        <code>List::map(normalize)</code>
        <span>|&gt;</span>
        <code>List::filter(is_valid)</code>
      </div>
    </div>

    <div class="khora-home-panel">
      <p class="khora-eyebrow">One toolchain</p>
      <h2>The compiler is the source of truth.</h2>
      <p>Build, test, format, generate API docs, run the language server, and expose compiler-backed intelligence to coding agents from the Khora toolchain.</p>
      <div class="khora-command-list" aria-label="Khora CLI commands">
        <code>khora check</code>
        <code>khora test</code>
        <code>khora fmt</code>
        <code>khora doc</code>
        <code>khora mcp</code>
      </div>
    </div>
  </section>

  <section class="khora-home-paths">
    <div class="khora-paths-heading">
      <p class="khora-eyebrow">Pick a path</p>
      <h2>Start where you are.</h2>
    </div>
    <div class="khora-path-list">
      <a href="/docs/getting-started/">
        <span class="khora-path-number">01</span>
        <span><strong>Build your first Khora program</strong><small>Install the toolchain, create a project, build, run, and test it.</small></span>
        <span aria-hidden="true">→</span>
      </a>
      <a href="/docs/guide/">
        <span class="khora-path-number">02</span>
        <span><strong>Learn the language</strong><small>Values, ADTs, pipelines, generics, traits, effects, resources, and fibers.</small></span>
        <span aria-hidden="true">→</span>
      </a>
      <a href="/docs/stdlib/">
        <span class="khora-path-number">03</span>
        <span><strong>Browse the standard library</strong><small>Generated API reference kept in sync with the compiler source.</small></span>
        <span aria-hidden="true">→</span>
      </a>
      <a href="/docs/migration/from-typescript-effect/">
        <span class="khora-path-number">04</span>
        <span><strong>Coming from Effect TypeScript?</strong><small>Map the concepts you already know onto Khora's direct-style model.</small></span>
        <span aria-hidden="true">→</span>
      </a>
    </div>
  </section>

  <section class="khora-home-cta">
    <div>
      <p class="khora-eyebrow">Ready to try it?</p>
      <h2>Write the program. Let the type tell the truth.</h2>
    </div>
    <a class="khora-cta-button" href="/docs/getting-started/">Get started <span aria-hidden="true">→</span></a>
  </section>
</div>

"use static"
// A module-level const the markup reads: the prerender module emits it above
// `render()`, so `<h1>{title}</h1>` is a build-time expression, not a runtime one.
const title = "About"

// The static route: no signal, no handler, no import. It ships HTML and 0 bytes of
// JavaScript, and `scripts/e2e.sh` fails the run if this page grows a `<script>`.
export default function About(props: Record<never, never>) {
  return (
    <main>
      <h1>{title}</h1>
      <p>A runai route declares its mode on the first line, so a page cannot drift into shipping JavaScript by accident.</p>
      <p>This page is prerendered at build time and served as plain HTML — the counter on the home page is the island.</p>
    </main>
  )
}

# End-to-end suite

The real binaries, driven from the outside over HTTP, as classes.

```bash
cd tests/e2e
npm install
npm test              # every spec, four at a time
npm run test:serial   # one at a time, for a clearer log
npx nole './specs/cache/**/*.test.ts'   # one phase
```

`npm test` builds the binaries first, through `pretest`, and runs the real
ones out of `target/debug`. `APERIO_SERVER_BIN` / `APERIO_CLIENT_BIN` point it
somewhere else.

Nothing builds from *inside* a run, and that is a rule rather than an
accident: `cargo build` while the suite is up relinks artifacts the other
phases are executing, and a binary replaced underneath a spawn fails in ways
that read as product bugs. Running `npx nole` directly skips `pretest`, so
build first if you have not.

## How it is put together

Three layers, and the only rule worth remembering is which one you are in.

**`lib/`, resources.** A server, a mock backend, a client, a TCP echo, a
WebSocket backend, an MQTT probe. Each is a factory returning a class, so
`extends AperioServerBase()` is a *distinct* class: nole keys a dependency
instance by class identity, so a distinct class means its own process on its
own port. Two specs naming the *same* class share one instance on purpose.

Every member of a resource is `_`-prefixed. Nole collects a class's public
methods as tests, including a class reached only as a dependency, so an
unprefixed `get()` would be reported as a passing test named
`CacheServer.get()`.

**`specs/<phase>/fixtures.ts`, this phase's resources.** Subclasses that say
what env, config or routes they need. No tests here either.

A server's environment is a constant in nineteen cases out of twenty-one, so
it is an argument rather than a method:

```ts
export class AuthServer extends AperioServerBase({
  env: { APERIO_SERVER_AUTH: 'demo:secret123' },
}) {}
```

The two that override `_env()` instead are the two that cannot be written
ahead of time: one names the port the instance was given, the other a path
inside the data directory it was handed. `super._env()` still reaches the
constant, so a subclass can add to it.

A client names its server and backend once, in the `extends` clause:

```ts
export class CacheClient extends ClientFor(() => CacheServer, () => CacheBackend) {
  _hostname() { return 'cache.e2e.local' }
  _env() { return { APERIO_HOSTNAME: 'cache.e2e.local' } }
}
```

`ClientOf(() => Server)` is the same for a client with no HTTP backend of its
own. A client that needs a third dependency, a scale hook, a TCP echo, a
second backend, extends `AperioClientBase` directly and declares them itself;
TypeScript cannot thread a generic dependency map through a mixin, so those
few say `_serverToken()` by hand.

**`specs/<phase>/*.test.ts`, the assertions.** Ordinary classes. A method is a
test, methods run in declaration order within a class, and `_` keeps a helper
to yourself.

## Two things that will bite

**Ports are per instance.** Nothing is pinned, so phases do not contend and
`--concurrency` works. What still contends is several spec classes sharing one
server *and changing it*: the base phase does, so its classes are chained with
`after:` rather than left to overlap. If you add a spec there, put it on the
chain.

**`fetch` silently drops the `Host` header.** Almost every assertion here picks
its tunnel by `Host`, so use `server._fetch()` (which is `node:http` under
`lib/http.ts`), never global `fetch`.

## What replaced what

This was a shell suite until it wasn't: `tests/e2e.sh` sourced a harness and
seventeen phase files, about 3,700 lines, with nineteen Python servers living
inside heredocs. It covered the same ground and its coverage of the Rust code
was the same to within a tenth of a point.

Three things are better here and they are worth knowing, because they are
what the next person should not give back:

- **Every phase runs on its own.** Six of the shell phases could not; they
  used a backend the first phase started, so looking at one meant running the
  ones before it.
- **Ports are per instance.** Nothing is pinned, so phases do not contend and
  `--concurrency` works: about 94 seconds became about 27.
- **The lifecycle belongs to the runner.** The old config phase carried a
  comment explaining that a forgotten `stop_server` took down the *next*
  phase. `cleanUp` cannot forget.

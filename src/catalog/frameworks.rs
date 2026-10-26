//! Frameworks: how pando recognises one, starts it, and tells it a port.
//!
//! A framework is one row in [`RULES`]. The files that identify a framework
//! are also the marker files `signals` looks for, derived from the rows, so
//! a new framework is recognised the moment its row exists.

/// How a framework is told which port to listen on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMechanism {
    /// An environment variable, named here.
    Env(&'static str),
    /// A placeholder already inside the command pando proposes.
    InCommand,
    /// Nothing pando knows about; the developer says.
    Ask,
}

/// One framework: how to recognise it, how to start it, and how it takes a
/// port. Data, not code paths — a wrong entry is one table row to fix.
#[derive(Debug, Clone, Copy)]
pub struct FrameworkRule {
    pub name: &'static str,
    /// Any one of these files identifies it.
    pub markers: &'static [&'static str],
    /// Any one of these substrings in a script body identifies it.
    pub script_markers: &'static [&'static str],
    pub port: PortMechanism,
    pub default_port: u16,
    /// The command to propose when the project has no script to run, or
    /// when its scripts only build assets. `{runner}` is replaced with the
    /// project's package or venv runner.
    pub command: Option<&'static str>,
    /// The flag that tells this framework its port, for an app whose own
    /// script pando runs rather than the command above: `pnpm dev --port
    /// 1234`. `{port}` is replaced with the role template. `None`
    /// for a framework that takes its port some other way — Django's
    /// positional `host:port` cannot be appended to somebody's script.
    pub port_flag: Option<&'static str>,
    /// What a project with one of the markers must also be before this
    /// rule claims it.
    pub guard: Guard,
    /// Whether the framework's own server is the app and a `package.json`
    /// beside it only builds the assets: Laravel's `vite`, a Rails app on
    /// vite_ruby. Its command then leads the scripts, which run the asset
    /// server, not the app.
    pub scripts_build_assets: bool,
    /// Any one of these in a script body runs this framework's build and
    /// not its server: a library's `vite build --watch` binds no port, and
    /// its CLI refuses the `--port` a server would be given.
    pub build_markers: &'static [&'static str],
    /// How long a process running this framework's server is proposed to
    /// get before its port has to be bound, for a framework whose cold
    /// start is known to outlast pando's default wait. `None` keeps the
    /// default.
    pub ready_timeout_s: Option<u64>,
    /// Whether what this framework serves is pages a browser opens, not
    /// an API: a web framework's app, never a language's own server.
    /// Detection gives the worktree's URL to an app that serves pages
    /// over one that is an API beside it.
    pub pages: bool,
    /// How this framework's app is reached when it runs on a phone, a
    /// tablet or a simulator rather than in a browser on this machine.
    /// `None` for every framework a browser here opens.
    pub device: Option<Device>,
}

/// A framework whose app runs on a device: what the developer has to be
/// told about reaching it, and how it is opened. The addresses are
/// templates, `{host}` and `{port}` the bundler's, which only a running
/// worktree knows.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    /// What the developer has to be told: every address pando gives is
    /// `127.0.0.1`, which a phone or a tablet cannot reach.
    pub note: &'static str,
    /// The role its bundler's port is named, where the port variable is
    /// all an answer says: not `web`, which is the role a browser opens.
    pub role: &'static str,
    /// The app that opens [`Device::url`]: the framework's own client.
    pub client: &'static str,
    /// The URL the client opens the app at.
    pub url: &'static str,
    /// The URL a development build of the app opens it at, `{scheme}` the
    /// scheme the build registers.
    pub development_build: &'static str,
    /// The dependency in the app's `package.json` that makes its builds
    /// development builds: with it, a development build opens the app,
    /// not [`Device::client`].
    pub development_client: &'static str,
    /// Where the scheme a development build registers comes from.
    pub scheme: Scheme,
    /// The command that opens a URL, `{url}`, on the booted iOS
    /// simulator, which shares this machine's `127.0.0.1`. Quoted: a
    /// development build's URL has a `?`, which zsh globs.
    pub simulator: &'static str,
    /// The command that opens `{url}` on a connected Android device or
    /// emulator: first it has the device's `127.0.0.1:{port}` reach this
    /// machine's, where the bundler listens. Quoted as the simulator's is.
    pub android: &'static str,
    /// Environment that turns off what a dev server is for, as `(variable,
    /// what it turns off)`: `doctor` names one a process sets.
    pub disabled_by: &'static [(&'static str, &'static str)],
    /// What in the app's directory a build compiles in, which the bundler
    /// cannot bring to a build made without it.
    pub native: Native,
    /// What a build installed on a simulator says about itself.
    pub installed: Installed,
}

/// What a development build installed on the iOS simulator says about
/// itself, read from its app bundle on the simulator's disk: nothing runs.
/// A build made for another SDK than the worktree's JavaScript needs
/// fails on the first call into native code the bundle expects.
#[derive(Debug, Clone, Copy)]
pub struct Installed {
    /// The JSON the framework puts in the app bundle, relative to the
    /// bundle's `<Name>.app`: the app's config as it was built, its keys
    /// those below [`Scheme::root`].
    pub config: &'static str,
    /// The keys, below the root, down to the app's iOS bundle identifier,
    /// which the build's config and the app's own share.
    pub bundle_id: &'static [&'static str],
    /// The keys, in the build's config, down to the SDK version it was
    /// built with: `55.0.0`.
    pub sdk: &'static [&'static str],
    /// The package whose major version is the SDK the app's JavaScript
    /// needs: installed in `node_modules`, else its range in
    /// `package.json`.
    pub sdk_package: &'static str,
}

/// The native code of an app a device runs: a branch that changes it
/// needs a build of its own, or the bundle it serves reaches for a module
/// the installed build lacks.
#[derive(Debug, Clone, Copy)]
pub struct Native {
    /// Directories that hold native code, at any depth below the app:
    /// `ios/`, and a local module's `modules/<name>/ios/`.
    pub dirs: &'static [&'static str],
    /// Files at the app's top whose settings a build bakes in.
    pub files: &'static [&'static str],
    /// The commands, run in the app's directory, that make and install a
    /// build of this worktree and point it at the bundler already running
    /// on `{port}`, as `(platform, command)`: one per platform the app is
    /// built for, the iOS simulator's first.
    pub builds: &'static [(&'static str, &'static str)],
}

impl Native {
    /// The build for the iOS simulator, the one a build installed there
    /// is replaced with: the first of [`Native::builds`].
    pub fn simulator_build(&self) -> &'static str {
        self.builds.first().map_or("", |(_, build)| build)
    }

    /// Whether `path`, relative to the app's directory, is native code.
    pub fn is_native(&self, path: &str) -> bool {
        self.files.contains(&path) || {
            let mut dirs = path.split('/');
            dirs.next_back();
            dirs.any(|dir| self.dirs.contains(&dir))
        }
    }
}

/// The scheme a development build registers: a name from the app's own
/// config, made a URI scheme, behind a prefix.
#[derive(Debug, Clone, Copy)]
pub struct Scheme {
    /// The JSON file, in the app's directory, that holds the name.
    pub manifest: &'static str,
    /// Where in that file the app's config is: `expo`. The config written
    /// as code has the same keys at its top.
    pub root: &'static [&'static str],
    /// The keys, below the root, down to the name: `slug`.
    pub key: &'static [&'static str],
    /// The app's config written as code, in the app's directory. pando
    /// never runs it: the name is read from it only as the string literal
    /// the key's last name is given (`slug: "shop"`), and only where every
    /// place that sets it gives the same one.
    pub code: &'static [&'static str],
    pub prefix: &'static str,
}

impl Scheme {
    /// The scheme a build of an app so named registers: the name's letters,
    /// digits, `+`, `-` and `.` (RFC 3986's scheme characters), lowercased,
    /// behind the prefix. `None` for a name with none of them.
    pub fn of(&self, name: &str) -> Option<String> {
        let kept: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            .collect();
        (!kept.is_empty()).then(|| format!("{}{}", self.prefix, kept.to_ascii_lowercase()))
    }

    /// What stands for the scheme where the manifest does not say it:
    /// `exp+<slug>`, for the developer or their agent to fill in.
    pub fn placeholder(&self) -> String {
        format!(
            "{}<{}>",
            self.prefix,
            self.key.last().copied().unwrap_or("name")
        )
    }
}

/// What an app's own manifests say about opening it, read from its
/// directory by whoever may read files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppManifest {
    /// The scheme its development build registers, when the manifest says.
    pub scheme: Option<String>,
    /// Whether it depends on [`Device::development_client`].
    pub development_client: bool,
}

/// A [`Device`]'s addresses for one running bundler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLinks {
    /// The app that opens `url`: the framework's client, or a development
    /// build of the app.
    pub client: &'static str,
    pub url: String,
    /// The command that opens `url` on the booted iOS simulator.
    pub simulator: String,
    /// The command that opens `url` on a connected Android device or
    /// emulator.
    pub android: String,
    pub development_build: String,
    /// Why `url` cannot be opened as it stands, when it cannot: a
    /// development build's link whose scheme no manifest said, so it holds
    /// [`Scheme::placeholder`]. The commands are printed with it, for the
    /// developer to fill in, and never run.
    pub unknown: Option<String>,
}

/// [`AppLinks::client`] for an app a development build opens.
pub const DEVELOPMENT_BUILD: &str = "its development build";

impl Device {
    /// The addresses of the app a bundler on `host` and `port` serves.
    /// With a development client among its dependencies, its development
    /// build is what opens it, and `url` is that build's.
    pub fn links(&self, host: &str, port: u16, app: &AppManifest) -> AppLinks {
        let fill = |template: &str| {
            template
                .replace("{host}", host)
                .replace("{port}", &port.to_string())
        };
        let scheme = app
            .scheme
            .clone()
            .unwrap_or_else(|| self.scheme.placeholder());
        let development_build = fill(self.development_build).replace("{scheme}", &scheme);
        let (client, url) = match app.development_client {
            true => (DEVELOPMENT_BUILD, development_build.clone()),
            false => (self.client, fill(self.url)),
        };
        let unknown = (app.development_client && app.scheme.is_none()).then(|| {
            let key = self.scheme.key.last().copied().unwrap_or("name");
            let path: Vec<&str> = self
                .scheme
                .root
                .iter()
                .chain(self.scheme.key)
                .copied()
                .collect();
            format!(
                "neither its {} nor a `{key}: \"…\"` literal in its config's code names its {}, \
                 and no build of it is on a booted simulator, so the scheme its development \
                 build registers is not known: `{}` stands for it",
                self.scheme.manifest,
                path.join("."),
                self.scheme.placeholder()
            )
        });
        AppLinks {
            client,
            simulator: self.simulator.replace("{url}", &url),
            android: fill(self.android).replace("{url}", &url),
            development_build,
            url,
            unknown,
        }
    }
}

/// What a marker match must also pass, for a marker file that more than
/// one kind of project has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// The marker file is enough.
    Marker,
    /// A crate that builds something runnable: a Cargo.toml with no binary
    /// is a library, and there is nothing to serve.
    BinaryCrate,
    /// A Go module with a `package main` to run: its root, or a command
    /// under `cmd/` that may be its server. A library has nothing for `go
    /// run` to run, a lone command named as a tool is no server, and
    /// among several commands none named as a server there is no telling
    /// which one serves.
    GoMain,
    /// One of these files contains this text: every Mix project has a
    /// `mix.exs`, and only one that depends on `:phoenix` is Phoenix.
    Mentions(&'static [&'static str], &'static str),
}

/// The rules pando ships with. Order matters: the first match wins, so the
/// specific frameworks come before the conventions they are built on, and
/// an app server before the asset pipeline it builds with.
pub const RULES: [FrameworkRule; 13] = [
    FrameworkRule {
        name: "Next.js",
        markers: &[
            "next.config.js",
            "next.config.mjs",
            "next.config.cjs",
            "next.config.ts",
        ],
        script_markers: &["next dev"],
        port: PortMechanism::Env("PORT"),
        default_port: 3000,
        command: Some("npx next dev"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Nuxt",
        markers: &["nuxt.config.ts", "nuxt.config.js", "nuxt.config.mjs"],
        script_markers: &["nuxt dev"],
        port: PortMechanism::Env("PORT"),
        default_port: 3000,
        command: Some("npx nuxt dev"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    // Astro sits on Vite but has its own CLI, its own default port and its
    // own command, so it comes before the Vite row that would claim it.
    FrameworkRule {
        name: "Astro",
        markers: &["astro.config.mjs", "astro.config.ts", "astro.config.js"],
        script_markers: &["astro dev"],
        port: PortMechanism::InCommand,
        default_port: 4321,
        command: Some("npx astro dev --port {port:web}"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Angular",
        markers: &["angular.json"],
        script_markers: &["ng serve"],
        port: PortMechanism::InCommand,
        default_port: 4200,
        command: Some("npx ng serve --port {port:web}"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: false,
        // A library's `ng build --watch`.
        build_markers: &["ng build"],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    // Expo, React Native's dev server. Metro serves a bundle to a device
    // or a simulator, not a page. Its CLI reads `--port`, then
    // `RCT_METRO_PORT`, then falls back to 8081, and never `PORT`. Its
    // template names the dev script `start`, not `dev`. With no terminal
    // it waits on no keypress, so it needs no `CI=1`, which would also
    // turn off its reloads and file watching. A cold Metro cache can take
    // well past pando's default wait to bind. The app runs on a device,
    // which reaches this machine only at its LAN address.
    FrameworkRule {
        name: "Expo",
        markers: &["app.json", "app.config.js", "app.config.ts"],
        script_markers: &["expo start"],
        port: PortMechanism::Env("RCT_METRO_PORT"),
        default_port: 8081,
        command: Some("npx expo start"),
        port_flag: Some("--port {port}"),
        // `app.json` is every Heroku app's too: only one that has an
        // `expo` key, or a manifest that depends on `expo`, is Expo.
        guard: Guard::Mentions(&["package.json", "app.json"], "\"expo\""),
        scripts_build_assets: false,
        // A static export, or the native projects generated for a build.
        build_markers: &["expo export", "expo prebuild"],
        ready_timeout_s: Some(90),
        pages: false,
        device: Some(Device {
            note: "a phone or tablet reaches Metro, and the app reaches its backend, at this \
                   machine's LAN address, not 127.0.0.1: REACT_NATIVE_PACKAGER_HOSTNAME and the \
                   EXPO_PUBLIC_* backend URL need it",
            role: "metro",
            client: "Expo Go",
            url: "exp://{host}:{port}",
            // Expo's own form: Metro's URL, encoded, as the query value.
            development_build: "{scheme}://expo-development-client/?url=http%3A%2F%2F{host}%3A{port}",
            development_client: "expo-dev-client",
            // `expo-dev-client` registers `exp+` and the slug, not
            // `expo.scheme` (its `getDefaultScheme`).
            scheme: Scheme {
                manifest: "app.json",
                root: &["expo"],
                key: &["slug"],
                // Every file Expo reads a dynamic config from.
                code: &[
                    "app.config.ts",
                    "app.config.js",
                    "app.config.mjs",
                    "app.config.cjs",
                ],
                prefix: "exp+",
            },
            simulator: "xcrun simctl openurl booted '{url}'",
            // `adb reverse` is what `expo run:android` sets up itself.
            android: "adb reverse tcp:{port} tcp:{port} && adb shell am start -a \
                      android.intent.action.VIEW -d '{url}'",
            // Expo's CLI reads it as any CI does, and Metro then says
            // "reloads are disabled".
            disabled_by: &[("CI", "Metro's reloads and file watching")],
            // A config plugin runs at build time, from the app config.
            native: Native {
                dirs: &["ios", "android"],
                files: &[
                    "app.json",
                    "app.config.js",
                    "app.config.ts",
                    "app.config.mjs",
                    "app.config.cjs",
                ],
                // `--port` with this app's Metro already on it reuses that
                // Metro rather than starting a second one; `--no-bundler`
                // beside it is refused as mutually exclusive.
                builds: &[
                    ("ios", "npx expo run:ios --port {port}"),
                    ("android", "npx expo run:android --port {port}"),
                ],
            },
            // `expo-constants` embeds the public config at build time;
            // its `sdkVersion` is the `expo` package's.
            installed: Installed {
                config: "EXConstants.bundle/app.config",
                bundle_id: &["ios", "bundleIdentifier"],
                sdk: &["sdkVersion"],
                sdk_package: "expo",
            },
        }),
    },
    // The app servers come before the Vite row: a Laravel app has a
    // vite.config.js and a `dev: vite` script, and so do Django and Rails
    // apps that build their assets with Vite.
    FrameworkRule {
        name: "Django",
        markers: &["manage.py"],
        script_markers: &[],
        port: PortMechanism::InCommand,
        default_port: 8000,
        command: Some("{runner}python manage.py runserver 127.0.0.1:{port:web}"),
        port_flag: None,
        guard: Guard::Marker,
        scripts_build_assets: true,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Rails",
        // Rails' own files. `config.ru` is every Rack app's and `bin/dev`
        // is a helper script in any language.
        markers: &["bin/rails", "config/application.rb"],
        script_markers: &[],
        port: PortMechanism::InCommand,
        default_port: 3000,
        command: Some("bin/rails server -p {port:web}"),
        port_flag: Some("-p {port}"),
        guard: Guard::Marker,
        scripts_build_assets: true,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Phoenix",
        markers: &["mix.exs"],
        script_markers: &[],
        port: PortMechanism::Env("PORT"),
        default_port: 4000,
        command: Some("mix phx.server"),
        port_flag: None,
        // `{:phoenix, …}` in the deps, or `:phoenix,` in the lockfile an
        // umbrella keeps at its root beside a `mix.exs` that has none.
        guard: Guard::Mentions(&["mix.exs", "mix.lock"], ":phoenix,"),
        scripts_build_assets: true,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Laravel",
        markers: &["artisan"],
        script_markers: &[],
        port: PortMechanism::InCommand,
        default_port: 8000,
        command: Some("php artisan serve --port {port:web}"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: true,
        build_markers: &[],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Vite",
        markers: &[
            "vite.config.ts",
            "vite.config.js",
            "vite.config.mjs",
            "vite.config.mts",
            "vite.config.cjs",
        ],
        // React Router and Remix's Vite mode are Vite underneath and take
        // the same `--port` flag.
        script_markers: &[
            "vite",
            "svelte-kit dev",
            "react-router dev",
            "remix vite:dev",
        ],
        // Vite reads PORT only through its config, so the flag is the
        // reliable route — and it is one pando can put in the command.
        port: PortMechanism::InCommand,
        default_port: 5173,
        command: Some("npx vite --port {port:web}"),
        port_flag: Some("--port {port}"),
        guard: Guard::Marker,
        scripts_build_assets: false,
        // Library mode's `vite build --watch`. Not `vite preview`, which
        // serves the build and takes `--port`.
        build_markers: &["vite build"],
        ready_timeout_s: None,
        pages: true,
        device: None,
    },
    FrameworkRule {
        name: "Go",
        markers: &["go.mod"],
        script_markers: &[],
        port: PortMechanism::Env("PORT"),
        default_port: 8080,
        // For a module whose root is a main package, which `go_main`
        // decides. One whose commands live under `cmd/` runs each by its
        // path, `go run ./cmd/<name>`, which `go_commands` lists.
        command: Some("go run ."),
        port_flag: None,
        guard: Guard::GoMain,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: false,
        device: None,
    },
    FrameworkRule {
        name: "Rust",
        markers: &["Cargo.toml"],
        script_markers: &[],
        port: PortMechanism::Env("PORT"),
        default_port: 8080,
        // Only for a crate that builds a binary; a library has nothing to
        // run, which `binary_crate` decides.
        command: Some("cargo run"),
        port_flag: None,
        guard: Guard::BinaryCrate,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: false,
        device: None,
    },
    FrameworkRule {
        name: "Node",
        markers: &[],
        script_markers: &["node ", "nodemon", "tsx ", "ts-node", "fastify", "express"],
        port: PortMechanism::Env("PORT"),
        default_port: 3000,
        command: None,
        port_flag: None,
        guard: Guard::Marker,
        scripts_build_assets: false,
        build_markers: &[],
        ready_timeout_s: None,
        pages: false,
        device: None,
    },
];

/// The [`FrameworkRule::device`] of a process that runs the app of a
/// framework a device reaches, with that framework's port variable: one
/// that takes its port from the variable, one of `port_vars`, or whose
/// command runs the framework's server by name.
pub fn device(port_vars: &[&str], cmd: &str) -> Option<(&'static Device, &'static str)> {
    RULES.iter().find_map(|rule| {
        let device = rule.device.as_ref()?;
        let PortMechanism::Env(var) = rule.port else {
            return None;
        };
        let by_port = port_vars.contains(&var);
        let by_cmd = rule
            .script_markers
            .iter()
            .any(|marker| cmd.contains(marker));
        (by_port || by_cmd).then_some((device, var))
    })
}

/// The role a port variable owns when it is the only one an answer names:
/// [`Device::role`] for the variable a device framework reads, `None` for
/// every other, which owns the browser's role.
pub fn device_role(var: &str) -> Option<&'static str> {
    device(&[var], "").map(|(device, _)| device.role)
}

/// [`Device::note`], for a process [`device`] recognises.
pub fn device_note(port_vars: &[&str], cmd: &str) -> Option<&'static str> {
    device(port_vars, cmd).map(|(device, _)| device.note)
}

/// Files that say something about the project's toolchain without
/// identifying a framework. Listed with the framework markers in
/// `signals`.
const TOOLCHAIN_MARKERS: [&str; 1] = ["pyproject.toml"];

/// Every marker file `signals` looks for: each rule's markers, in rule
/// order, then the toolchain markers.
pub fn marker_files() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let rules = RULES.iter().flat_map(|rule| rule.markers.iter().copied());
    for marker in rules.chain(TOOLCHAIN_MARKERS) {
        if !out.contains(&marker) {
            out.push(marker);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_marker_is_looked_for() {
        let markers = marker_files();
        for rule in RULES {
            for marker in rule.markers {
                assert!(markers.contains(marker), "{} marker {marker}", rule.name);
            }
        }
    }

    // A device is found through the rule's port variable, so a rule that
    // takes its port some other way could never print its note or give
    // its app's links.
    #[test]
    fn a_rule_with_a_device_takes_its_port_from_a_variable() {
        for rule in RULES.iter().filter(|rule| rule.device.is_some()) {
            assert!(
                matches!(rule.port, PortMechanism::Env(_)),
                "{} has a device note and no port variable",
                rule.name
            );
        }
    }

    #[test]
    fn a_device_note_is_expos_alone() {
        let expo = device_note(&["RCT_METRO_PORT"], "npm run start").expect("Expo's note");
        assert!(expo.contains("REACT_NATIVE_PACKAGER_HOSTNAME"), "{expo}");
        assert_eq!(device_note(&[], "npx expo start"), Some(expo));
        assert_eq!(device_note(&["PORT"], "npm run dev"), None);
        assert_eq!(device_note(&[], "npx next dev"), None);
    }

    // Expo's app opens in Expo Go at `exp://`, on the simulator through
    // `simctl`, and in a development build through the scheme the build
    // registers, with Metro's URL encoded as the query value Expo's CLI
    // writes.
    #[test]
    fn expos_app_is_opened_by_its_links() {
        let (expo, var) = device(&["RCT_METRO_PORT"], "").expect("Expo's device");
        assert_eq!(var, "RCT_METRO_PORT");
        let links = expo.links("127.0.0.1", 8123, &AppManifest::default());
        assert_eq!(links.client, "Expo Go");
        assert_eq!(links.url, "exp://127.0.0.1:8123");
        assert_eq!(
            links.simulator,
            "xcrun simctl openurl booted 'exp://127.0.0.1:8123'"
        );
        assert_eq!(
            links.android,
            "adb reverse tcp:8123 tcp:8123 && adb shell am start -a \
             android.intent.action.VIEW -d 'exp://127.0.0.1:8123'"
        );
        assert_eq!(
            links.development_build,
            "exp+<slug>://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A8123"
        );
        // Every placeholder of every row is filled.
        for rule in RULES.iter().filter_map(|rule| rule.device.as_ref()) {
            let links = rule.links("127.0.0.1", 1, &AppManifest::default());
            for filled in [
                &links.url,
                &links.simulator,
                &links.android,
                &links.development_build,
            ] {
                assert!(!filled.contains('{'), "{filled}");
            }
        }
    }

    // An app with `expo-dev-client` is opened by its development build,
    // which is what the simulator command then opens.
    #[test]
    fn a_development_build_opens_an_app_that_depends_on_the_client() {
        let (expo, _) = device(&["RCT_METRO_PORT"], "").expect("Expo's device");
        let app = AppManifest {
            scheme: expo.scheme.of("DriveeSafeCall"),
            development_client: true,
        };
        let links = expo.links("127.0.0.1", 8123, &app);
        let url = "exp+driveesafecall://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A8123";
        assert_eq!(links.client, DEVELOPMENT_BUILD);
        assert_eq!(links.url, url);
        assert_eq!(links.development_build, url);
        assert_eq!(
            links.simulator,
            format!("xcrun simctl openurl booted '{url}'")
        );
        assert_eq!(links.unknown, None);
        // Expo Go's link is always whole.
        assert_eq!(
            expo.links("127.0.0.1", 8123, &AppManifest::default())
                .unknown,
            None
        );

        // An app configured in `app.config.ts` alone: no manifest names
        // the slug, so its link holds the placeholder, and says so.
        let app = AppManifest {
            scheme: None,
            development_client: true,
        };
        let links = expo.links("127.0.0.1", 8123, &app);
        assert!(links.url.starts_with("exp+<slug>://"), "{}", links.url);
        assert_eq!(
            links.unknown.as_deref(),
            Some(
                "neither its app.json nor a `slug: \"…\"` literal in its config's code names \
                 its expo.slug, and no build of it is on a booted simulator, so the scheme its \
                 development build registers is not known: `exp+<slug>` stands for it"
            )
        );
    }

    // A native module's own directory is native at any depth; a screen's
    // source, or a directory merely named like one, is not.
    #[test]
    fn expos_native_code_is_its_native_directories_and_app_config() {
        let (expo, _) = device(&["RCT_METRO_PORT"], "").expect("Expo's device");
        for path in [
            "ios/Podfile",
            "android/app/build.gradle",
            "modules/call/ios/CallModule.swift",
            "app.json",
            "app.config.ts",
        ] {
            assert!(expo.native.is_native(path), "{path}");
        }
        for path in [
            "app/index.tsx",
            "src/ios.ts",
            "ios",
            "docs/app.json",
            "package.json",
        ] {
            assert!(!expo.native.is_native(path), "{path}");
        }
    }

    // One build per platform, the iOS simulator's first, which `status
    // --json` keeps as its `build`. Expo refuses `--port` beside
    // `--no-bundler`; `--port` alone reuses the worktree's Metro.
    #[test]
    fn expos_builds_are_one_per_platform_on_the_worktrees_metro() {
        let (expo, _) = device(&["RCT_METRO_PORT"], "").expect("Expo's device");
        assert_eq!(
            expo.native.builds,
            [
                ("ios", "npx expo run:ios --port {port}"),
                ("android", "npx expo run:android --port {port}"),
            ]
        );
        for rule in RULES.iter().filter_map(|rule| rule.device.as_ref()) {
            assert_eq!(rule.native.builds.first().map(|b| b.0), Some("ios"));
            for (platform, build) in rule.native.builds {
                assert!(build.contains("{port}"), "{platform}: {build}");
                assert!(!build.contains("--no-bundler"), "{platform}: {build}");
            }
        }
    }

    // Every file the scheme's name is read from is a file a build bakes
    // in, and the other way round: one set of config files, named twice
    // because one list is read and the other watched.
    #[test]
    fn a_devices_config_files_are_its_native_files() {
        for device in RULES.iter().filter_map(|rule| rule.device.as_ref()) {
            let mut read: Vec<&str> = std::iter::once(device.scheme.manifest)
                .chain(device.scheme.code.iter().copied())
                .collect();
            let mut watched = device.native.files.to_vec();
            read.sort_unstable();
            watched.sort_unstable();
            assert_eq!(read, watched);
        }
    }

    // Expo's own `getDefaultScheme`: the slug's scheme characters,
    // lowercased, behind `exp+`.
    #[test]
    fn a_development_builds_scheme_is_its_slug_made_a_scheme() {
        let (expo, _) = device(&["RCT_METRO_PORT"], "").expect("Expo's device");
        assert_eq!(
            expo.scheme.of("hello-world").as_deref(),
            Some("exp+hello-world")
        );
        assert_eq!(expo.scheme.of("My App_2").as_deref(), Some("exp+myapp2"));
        assert_eq!(expo.scheme.of("!!!"), None);
        assert_eq!(expo.scheme.placeholder(), "exp+<slug>");
    }

    #[test]
    fn rule_names_are_unique() {
        for (i, rule) in RULES.iter().enumerate() {
            assert!(
                RULES[i + 1..].iter().all(|other| other.name != rule.name),
                "{} is listed twice",
                rule.name
            );
        }
    }
}

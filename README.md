Knot Tangle and Twine are works in progress. The spec and reference implementations are still being built out. This repo serves docs and demo of the development. Stay tuned.

# Knot

Knot is a programming language designed for programmatic CAD applications. Designs are represented as knot programs running in an application specific runtime. 

Knot is essentially a generous subset of Haskell with record syntax from Elm and annotations. 

Knot omits many of haskell's advanced type featurese but keeps typeclasses. 

Knot's novel addition is its *unravel* system:

Unravels are composeable pullbacks annotated on knot functions allowing a program to "unravel" target output values into source changes. 

All knot builtins come with their own unravels which can compose to an unravel for any function. However such unravels are usually not solveable. Instead, agentically authored program-specific unravels allow customizing how outputs unravel into source code on a per program basis. Together with *whips* which bind output objects with unravel entry points, you both author the porgram and the UI to unravel and modify the program together!!

The knot language is described in [language-spec-notes.md](language-spec-notes.md) but it is essentially as described above. The unravel system is described in [unravel-design.md](unravel-design.md) and is the more interesting document.


# Tangle 

Knot programs can be alternatively be visualized in Tangle, the node graph representation of knot.

Tangle provides an injective reverse mapping to the knot source code thus edits can be made in the Tangle node graph UI while ultimately still represented as knot program. 

Most knot language features can be represented in Tangle. Some features are omitted by design as they aren't useful to visualize or modify in Knot.

Tangle is intended as a high level visualization of the underlying program. Programs are likely to be agentically written and such a visualization is helpful for both programmers and non programmers alike.


# Twine 

Twine is the runtime engine for knot programs. Applications host the twine engine and inject their own types and functions so that knot programs can generate output for the application.

Twine must also host the unravel engine to generate source edits and efficiently revaluate changes in realtime. Towards this end, Twine is carefully optimized with source tracking, JIT compliation, and caching for partial reevaluation in order to enable this. 

Application specific knot programs can declare *whips* which declare a UI object that binding an unravel entry point to the objects created by the program.

## Try the demos

[Open the gallery](https://minimapletinytools.github.io/knotty/) or go straight to the [Tangle storybook](https://minimapletinytools.github.io/knotty/tangle/).

The drawing demos run Twine in a browser worker. Select a shape and change a property to see the drawing and its Knot source update together. **Source** opens the read-only Monaco pane. **Tangle** opens a graph of the current drawing. Choose a definition and edit a number, connect a rope or reposition a node. Finishing the edit updates the drawing. **Undo** reverses an accepted edit. Reloading a demo restores the original drawing.

### Run locally

From the repository root, with Rust and Node.js 24 installed:

```sh
cd knotty-drawings/protoapp
npm ci
rustup target add wasm32-wasip1
npm run pages
python3 -m http.server 8000 --directory ../../dist
```

Open <http://localhost:8000/>. For the native editor, copy a drawing to a working file and run the app in the same directory. Native edits are saved to that file.

```sh
npm run build
cp ../example-drawing.knot /tmp/ExampleDrawing.knot
cargo run --release --locked -- /tmp/ExampleDrawing.knot --renderer direct
```

### GitHub Pages

Select **GitHub Actions** in the repository’s **Settings → Pages → Build and deployment**. The Pages workflow builds and tests pull requests and deploys pushes to `main`. You can also run it from the Actions tab.

## License

Knotty is licensed under the GNU General Public License, version 3 only. [Full license](LICENSE)

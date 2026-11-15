import { render } from "preact";
import { useEffect } from "preact/hooks";
import { Canvas } from "./Canvas.tsx";
import { HelpSheet } from "./Help.tsx";
import { Inspector } from "./Inspector.tsx";
import { installKeys } from "./keys.ts";
import { OpenScreen } from "./OpenScreen.tsx";
import { PageList } from "./PageList.tsx";
import { project, railOpen } from "./state.ts";
import { TitleBar, Toolbar } from "./TopBar.tsx";

function App() {
  useEffect(installKeys, []);

  if (!project.value) {
    return (
      <div class="app" data-screen="open">
        <TitleBar />
        <OpenScreen />
      </div>
    );
  }
  return (
    <div class="app" data-rail={railOpen.value ? "open" : "closed"}>
      <TitleBar />
      <Toolbar />
      {railOpen.value && <PageList />}
      <Canvas />
      <Inspector />
      <HelpSheet />
    </div>
  );
}

render(<App />, document.getElementById("app")!);

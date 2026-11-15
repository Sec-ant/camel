import { render } from "preact";
import { useEffect } from "preact/hooks";
import { Canvas } from "./Canvas.tsx";
import { HelpSheet } from "./Help.tsx";
import { Inspector } from "./Inspector.tsx";
import { JobsScreen } from "./Jobs.tsx";
import { installKeys } from "./keys.ts";
import { OpenScreen } from "./OpenScreen.tsx";
import { PageList } from "./PageList.tsx";
import { SettingsScreen } from "./Settings.tsx";
import { project, railOpen, view } from "./state.ts";
import { TitleBar, Toolbar } from "./TopBar.tsx";

function App() {
  useEffect(installKeys, []);

  // Screens other than the workbench keep their place in the tree hidden, so a project's
  // workbench state (and an open screen's choices) survive a visit to Jobs or Settings.
  const screen = view.value;
  return (
    <>
      {screen === "settings" && (
        <div class="app" data-screen="form">
          <TitleBar />
          <SettingsScreen />
        </div>
      )}
      {screen === "jobs" && project.value && (
        <div class="app" data-screen="form">
          <TitleBar />
          <JobsScreen />
        </div>
      )}
      {!project.value ? (
        <div class="app" data-screen="open" hidden={screen !== "main"}>
          <TitleBar />
          <OpenScreen />
        </div>
      ) : (
        <div class="app" data-rail={railOpen.value ? "open" : "closed"} hidden={screen !== "main"}>
          <TitleBar />
          <Toolbar />
          {railOpen.value && <PageList />}
          <Canvas />
          <Inspector />
          <HelpSheet />
        </div>
      )}
    </>
  );
}

render(<App />, document.getElementById("app")!);

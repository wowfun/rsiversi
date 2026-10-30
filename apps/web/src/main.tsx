import { mountWorkbench } from "./workbench-root.tsx";
const owner = mountWorkbench(document.getElementById("root")!);
owner.runtime.start();

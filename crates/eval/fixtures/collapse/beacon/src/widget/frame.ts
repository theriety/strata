// Widget frame around a panel: beacon package, widget domain.
import { Panel } from "./panel";

export class Frame {
  constructor(private readonly panel: Panel) {}

  render(): string {
    return `frame(${this.panel.label()})`;
  }
}

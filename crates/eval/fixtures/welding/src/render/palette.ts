// Palette built on top of the canvas.
import { Canvas } from "./canvas";

export class Palette {
  constructor(private readonly canvas: Canvas) {}

  swatch(color: string): string {
    return this.canvas.draw(`swatch:${color}`);
  }
}

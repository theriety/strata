import { Shape, describeShape } from "./shape";

export class Rectangle implements Shape {
  constructor(
    private readonly width: number,
    private readonly height: number,
  ) {}

  area(): number {
    return this.width * this.height;
  }
}

export function summarize(rectangle: Rectangle): string {
  return describeShape(rectangle);
}

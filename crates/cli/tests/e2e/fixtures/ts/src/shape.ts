export interface Shape {
  area(): number;
}

export function describeShape(shape: Shape): string {
  return `area=${shape.area()}`;
}

export function getCurrentWindow() {
  return {
    minimize: () => Promise.resolve(),
    close: () => Promise.resolve(),
    startDragging: () => Promise.resolve(),
    startResizeDragging: () => Promise.resolve(),
  };
}

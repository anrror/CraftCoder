import '@testing-library/jest-dom/vitest';

// jsdom does not implement scrollIntoView — polyfill as noop
Element.prototype.scrollIntoView = () => {};

// jsdom does not implement matchMedia
Object.defineProperty(globalThis, 'matchMedia', {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }),
});

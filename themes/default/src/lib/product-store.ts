import { createSignal } from "solid-js";

/**
 * Product page state shared by the buy box and the gallery (separate islands, one module):
 * choosing a colour shows that variant's first photo.
 */
const [imageIndex, setImageIndex] = createSignal<number | null>(null);

export { imageIndex, setImageIndex };

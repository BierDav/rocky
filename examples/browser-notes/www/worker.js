import init, { Notes } from "./pkg/browser_notes.js";

const ready = init().then(() => Notes.open(location.hash.slice(1) || undefined));
const show = async (notes) => postMessage({ notes: await notes.list() });

ready.then((notes) => {
  show(notes);
  notes.watch(() => show(notes));
}).catch((error) => postMessage({ error: String(error) }));

onmessage = async ({ data }) => {
  try {
    await (await ready).add(data.add);
  } catch (error) {
    postMessage({ error: String(error) });
  }
};

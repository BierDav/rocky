import init, { Notes, newPersonPhrase } from "./pkg/browser_notes.js";

const show = async (notes) => postMessage({ notes: await notes.list() });
const open = async (phrase) => {
  await init();
  if (!phrase) {
    phrase = newPersonPhrase();
    postMessage({ phrase });
  }
  const notes = await Notes.open(phrase, location.hash.slice(1) || undefined);
  show(notes);
  notes.watch(() => show(notes));
  return notes;
};

let ready;
onmessage = async ({ data }) => {
  try {
    if ("phrase" in data) {
      ready = open(data.phrase);
      await ready;
    } else {
      await (await ready).add(data.add);
    }
  } catch (error) {
    postMessage({ error: String(error) });
  }
};

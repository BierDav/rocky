const scenario = await dioxus.recv();
const wait = async (predicate) => {
    if (predicate()) return;
    await new Promise((resolve, reject) => {
        const observer = new MutationObserver(() => {
            if (predicate()) { observer.disconnect(); clearTimeout(deadline); resolve(); }
        });
        const deadline = setTimeout(() => { observer.disconnect(); reject(new Error('UI condition timed out: ' + document.body.textContent)); }, 15000);
        observer.observe(document, {subtree: true, childList: true, attributes: true, characterData: true});
    });
};
const panel = (i) => document.querySelector(`#panel-${i}`);
const rows = (i) => [...panel(i).querySelectorAll('li')];
const title = (row) => row.querySelector('input[type=text]');
const rev = (i) => Number(panel(i).textContent.match(/revision (\d+)/)?.[1] ?? -1);
const find = (i, text) => rows(i).find(row => title(row).value === text);
const setText = (input, text) => {
    input.value = '';
    for (let length = 1; length <= text.length; length++) {
        input.value = text.slice(0, length);
        input.dispatchEvent(new Event('input', {bubbles: true}));
    }
};
const create = async (i, text) => {
    const input = document.querySelector(`#new-title-${i}`);
    setText(input, text);
    await wait(() => !input.form.querySelector('button').disabled);
    const before = rev(i);
    input.form.requestSubmit();
    await wait(() => rev(i) > before && find(i, text) && !input.disabled && input.value === '');
};
const rename = async (i, old, text) => {
    const row = find(i, old);
    const input = title(row);
    setText(input, text);
    await wait(() => !input.form.querySelector('button').disabled);
    const before = rev(i);
    input.form.requestSubmit();
    await wait(() => rev(i) > before && find(i, text) && !title(find(i, text)).disabled);
};
const toggle = async (connected) => {
    document.querySelector('#connection').click();
    await wait(() => document.querySelector('#connection').textContent.includes(connected ? 'Pause' : 'Resume'));
};
try {
    await wait(() => panel(0) && panel(1) && rev(0) >= 0 && rev(1) >= 0);
    if (scenario === 'publication') {
        await create(0, 'Sealed native todo');
        await wait(() => find(1, 'Sealed native todo'));
        document.querySelector('#publish-1').click();
        await wait(() => [0,1].every(i => panel(i).querySelectorAll('.publication').length === 1 && panel(i).textContent.includes('applied by named peer')));
        await rename(0, 'Sealed native todo', 'Live edit after publication');
        await wait(() => find(1, 'Live edit after publication'));
        if (![0,1].every(i => panel(i).querySelector('.publication').textContent.includes('Sealed native todo'))) throw new Error('publication changed with live edit');
        document.querySelector('#coordination').click();
        await wait(() => document.querySelector('#coordination').textContent.includes('Enable'));
        document.querySelector('#publish-1').click();
        await wait(() => panel(1).textContent.includes('Publication request 2:'));
        await create(1, 'Later offline input');
        await wait(() => find(0, 'Later offline input'));
        if (![0,1].every(i => panel(i).querySelectorAll('.publication').length === 1)) throw new Error('unconfirmed result exposed');
        document.querySelector('#coordination').click();
        await wait(() => [0,1].every(i => panel(i).querySelectorAll('.publication').length === 2));
        if ([0,1].some(i => panel(i).querySelectorAll('.publication')[1].textContent.includes('Later offline input'))) throw new Error('sealed input replaced');
        document.querySelector('#takeover').click();
        await wait(() => document.body.textContent.includes('Executor: Phone'));
        document.querySelector('#publish-0').click();
        await wait(() => [0,1].every(i => panel(i).querySelectorAll('.publication').length === 3 && panel(i).textContent.includes('applied by named peer')));
    } else if (scenario === 'offline') {
        await create(0, 'Shared todo');
        await wait(() => find(1, 'Shared todo'));
        await toggle(false);
        await rename(0, 'Shared todo', 'Offline title');
        const before = rev(1);
        find(1, 'Shared todo').querySelector('input[type=checkbox]').click();
        await wait(() => rev(1) > before && !title(find(1, 'Shared todo')).disabled);
        await wait(() => [0, 1].every(i => /Outgoing pending: [1-9]/.test(panel(i).textContent)));
        await toggle(true);
        await wait(() => [0, 1].every(i => find(i, 'Offline title')?.querySelector('input[type=checkbox]').checked));
        await wait(() => [0, 1].every(i => panel(i).textContent.includes('Outgoing pending: 0')));
        await toggle(false);
        await rename(0, 'Offline title', 'Laptop competing title');
        await rename(1, 'Offline title', 'Phone competing title');
        await create(0, 'Delete offline');
        const revision = rev(0);
        find(0, 'Delete offline').querySelector('button[type=button]').click();
        await wait(() => rev(0) > revision && rows(0).length === 1);
        await create(1, 'Phone survivor');
        if (!find(0, 'Laptop competing title') || !find(1, 'Phone competing title') || rows(1).length !== 2) throw new Error('offline lists differ');
    } else if (scenario === 'reopen') {
        await wait(() => [0, 1].every(i => rows(i).length === 2 && find(i, 'Phone competing title')?.querySelector('input[type=checkbox]').checked && find(i, 'Phone survivor')));
        await wait(() => [0, 1].every(i => panel(i).textContent.includes('Outgoing pending: 0')));
        if ([0, 1].some(i => find(i, 'Delete offline'))) throw new Error('deletion was revived');
    } else throw new Error('unknown scenario ' + scenario);
    dioxus.send('passed');
} catch (error) { dioxus.send(String(error)); }

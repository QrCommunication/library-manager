import { mount } from 'svelte';
import App from './App.svelte';
import { locale, setLanguage } from './lib/i18n';
import './app.css';

const target = document.getElementById('app');
if (!target) throw new Error('Library Manager cannot start: the #app element is missing.');

setLanguage('system', navigator.language);
locale.subscribe((language) => { document.documentElement.lang = language; });

function renderStartupFailure(root: HTMLElement): void {
  const main = document.createElement('main');
  main.className = 'page';
  const section = document.createElement('section');
  section.className = 'empty-state';
  section.setAttribute('role', 'alert');
  const heading = document.createElement('h1');
  heading.textContent = 'Library Manager';
  const french = document.createElement('p');
  french.lang = 'fr';
  french.textContent = 'L’interface n’a pas pu démarrer. Fermez puis relancez Library Manager.';
  const english = document.createElement('p');
  english.lang = 'en';
  english.textContent = 'The interface could not start. Close and reopen Library Manager.';
  section.append(heading, french, english);
  main.append(section);
  root.replaceChildren(main);
}

try {
  mount(App, { target });
} catch {
  renderStartupFailure(target);
}

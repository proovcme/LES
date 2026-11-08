import { mount } from 'svelte';
import App from './App.svelte';
import StatusPage from './components/StatusPage.svelte';
import './style.css';

const target = document.getElementById('app')!;
if (new URLSearchParams(location.search).get('view') === 'status') {
  mount(StatusPage, { target });
} else {
  mount(App, { target });
}

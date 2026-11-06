import {counted, documentSpecies, hash, treePosition} from './forest-model.js?v=800.2';

const radius = 64;
const spacing = 151;
const maximumTrees = 900;
const initialCamera = {yaw: -.24, pitch: .91, zoom: 1};
const clamp = (number, min, max) => Math.min(max, Math.max(min, number));
const crowns = {
  text: {profile:[[.34,.11],[.50,.32],[.70,.36],[.91,.22],[1,0]], hue:83, phase:0},
  table: {profile:[[.34,.08],[.50,.16],[.72,.20],[.92,.12],[1,0]], hue:104, phase:0},
  slides: {profile:[[.38,0],[.64,.32],[.78,.28],[1,0]], hue:28, phase:Math.PI/6},
  image: {profile:[[.16,.10],[.37,.16],[.73,.12],[.94,.06],[1,0]], hue:170, phase:0},
  other: {profile:[[.24,.14],[.42,.29],[.59,.24],[.84,.15],[1,0]], hue:91, phase:0},
};

export function groveLayout(catalog, dataset) {
  const documents = new Map();
  for (const doc of catalog.documents) {
    if (!documents.has(doc.dataset_id)) documents.set(doc.dataset_id, []);
    documents.get(doc.dataset_id).push(doc);
  }
  const groups = catalog.datasets.filter(grove => !dataset || grove.id === 'ds:' + dataset);
  const columns = Math.ceil(Math.sqrt(groups.length || 1));
  const rows = Math.ceil(groups.length / columns);
  const groves = groups.map((grove, index) => {
    const id = grove.id.replace(/^ds:/, '');
    const docs = documents.get(id) || [];
    docs.sort((a, b) => a.id.localeCompare(b.id));
    return {
      id, label: grove.label, count: docs.length,
      entries: docs.map((doc, index) => ({doc, position:treePosition(doc, index, docs.length), species:documentSpecies(doc)})),
      x: (index % columns - (columns - 1) / 2) * spacing,
      z: (Math.floor(index / columns) - (rows - 1) / 2) * spacing,
    };
  });
  return {groves, extent: Math.max(96, (Math.hypot(columns - 1, rows - 1) * spacing) / 2 + radius)};
}

export function projection(camera, width, height, extent) {
  const yawX = Math.cos(camera.yaw), yawZ = Math.sin(camera.yaw);
  const up = Math.cos(camera.pitch), down = Math.sin(camera.pitch);
  const top = 145, bottom = height - 108;
  const scale = Math.min((width - 28) / (extent * 2), (bottom - top) / (extent * 2)) * camera.zoom;
  return (x, y, z) => {
    const right = x * yawX - z * yawZ;
    const forward = x * yawZ + z * yawX;
    const depth = forward * up + y * down;
    const factor = Math.max(.02, scale) / (1 - depth / (extent * 5));
    return {x: width / 2 + right * factor, y: (top + bottom) / 2 + (forward * down - y * up) * factor, depth};
  };
}

function polygon(ctx, vertices, color, stroke) {
  ctx.beginPath();
  ctx.moveTo(vertices[0].x, vertices[0].y);
  for (const p of vertices.slice(1)) ctx.lineTo(p.x, p.y);
  ctx.closePath();
  ctx.fillStyle = color;
  ctx.fill();
  if (stroke) {
    ctx.strokeStyle = stroke;
    ctx.stroke();
  }
}

function contains(vertices, x, y) {
  return vertices.reduce((inside, a, index) => {
    const b = vertices[(index + 1) % vertices.length];
    if ((a.y > y) === (b.y > y)) return inside;
    return x < a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y) ? !inside : inside;
  }, false);
}

function ring(project, x, y, z, size, sides, phase = 0) {
  return Array.from({length: sides}, (_, index) => {
    const angle = index * Math.PI * 2 / sides + phase;
    return project(x + Math.cos(angle) * size, y, z + Math.sin(angle) * size);
  });
}

function drawGrove(ctx, project, grove, sides) {
  const top = ring(project, grove.x, 0, grove.z, radius, sides);
  if (sides === 8) {
    polygon(ctx, top, '#c5d6a6', '#859d6b');
    return top;
  }
  const bottom = ring(project, grove.x, -8, grove.z, radius, sides);
  const left = project(grove.x - radius, 0, grove.z), right = project(grove.x + radius, 0, grove.z);
  const shadow = bottom.map(p => ({x:p.x + 4, y:p.y + 8}));
  polygon(ctx, shadow, '#2a49241c');
  const faces = top.map((point, index) => {
    const next = (index + 1) % top.length;
    return {depth: (point.depth + top[next].depth) / 2,
      vertices: [point, top[next], bottom[next], bottom[index]]};
  }).sort((a, b) => a.depth - b.depth);
  for (const {vertices, depth} of faces) {
    polygon(ctx, vertices, depth > grove.center.depth ? '#98a778' : '#a9b88b');
  }
  const fill = ctx.createLinearGradient(left.x, left.y, right.x, right.y);
  fill.addColorStop(0, '#e6edc9');
  fill.addColorStop(1, '#b4c993');
  polygon(ctx, top, fill, '#859d6b');
  return top;
}

function drawTree(ctx, project, tree, chosen, detail) {
  const {x, z, height, doc, species} = tree;
  const base = project(x, 0, z), top = project(x, height, z);
  const size = Math.hypot(top.x - base.x, top.y - base.y);
  if (chosen) {
    polygon(ctx, ring(project, x, .2, z, height * .34, 16), '#f7f4dc', '#266345');
  }
  const trunk = ring(project, x, 0, z, height * .05, 4, Math.PI / 4);
  const trunkTop = ring(project, x, height * .43, z, height * .045, 4, Math.PI / 4);
  const front = trunk.reduce((best, point, index) => point.depth > trunk[best].depth ? index : best, 0);
  for (const side of [(front + 3) % 4, front]) {
    const next = (side + 1) % 4;
    const bark = species === 'table' ? ['#c2c1a8','#f4eedb'] : ['#7c6949','#9b865b'];
    polygon(ctx, [trunk[side],trunk[next],trunkTop[next],trunkTop[side]], bark[Number(side !== front)]);
  }
  const green = hash(doc.id) % 15;
  const crown = crowns[species];
  const pick = {doc, x:base.x, y:(base.y + top.y) / 2, top:top.y, r:Math.max(7, size * .54)};
  if (crown) {
    drawCrown(ctx, project, tree, crown, detail, green);
    return pick;
  }
  const tiers = detail ? 3 : 2;
  for (let tier = 0; tier < tiers; tier++) {
    const y = height * (.26 + tier * .21);
    const corners = ring(project, x, y, z, height * (.24 - tier * .05), 4, Math.PI / 4);
    const tip = project(x, tier === tiers - 1 ? height : y + height * .49, z);
    const faces = corners.map((point, index) => {
      const next = corners[(index + 1) % corners.length];
      return {depth: (point.depth + next.depth) / 2, vertices:[point, next, tip], light:[3, 7, -4, -1][index]};
    }).sort((a, b) => a.depth - b.depth);
    for (const {vertices, light} of faces) {
      polygon(ctx, vertices, `hsl(${139 + green / 2} ${35 + green}% ${24 + green / 3 + tier * 3 + light}%)`);
    }
  }
  return pick;
}

function drawCrown(ctx, project, {x, z, height}, {profile, hue, phase}, detail, variation) {
  const sides = detail ? 6 : 4;
  const rings = profile.map(([y, width]) => ring(project, x, y * height, z, width * height, sides, phase));
  const faces = [];
  for (let level = 1; level < rings.length; level++) {
    const lower = rings[level - 1], upper = rings[level];
    for (let side = 0; side < sides; side++) {
      const next = (side + 1) % sides;
      const vertices = [lower[side], lower[next], upper[next], upper[side]];
      faces.push({
        vertices, depth:vertices.reduce((sum, p) => sum + p.depth, 0) / 4,
        light: 27 + level * 3 + 5 * Math.cos(side * Math.PI * 2 / sides) + variation / 3,
      });
    }
  }
  faces.sort((a, b) => a.depth - b.depth);
  for (const {vertices, light} of faces) {
    polygon(ctx, vertices, `hsl(${hue + variation / 3} ${hue < 40 ? 53 : 40}% ${light}%)`);
  }
}

function visible(point, w, h, padding) {
  return point.x >= -padding && point.x <= w + padding && point.y >= -padding && point.y <= h + padding;
}

function drawDocumentLabels(ctx, picks, groveLabels, selected, hovered, zoom, width, height) {
  if (zoom < 1.75 && !selected && !hovered) return [];
  const labels = [];
  const occupied = [...groveLabels];
  const overlaps = (a, b) => a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
  ctx.font = '600 12px "Segoe UI",sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  const ordered = picks.slice().reverse().sort((a, b) => priority(b) - priority(a));
  function priority(pick) {
    return pick.doc.id === hovered ? 2 : pick.doc.id === selected ? 1 : 0;
  }
  for (const pick of ordered) {
    const active = priority(pick) > 0;
    if (!active && (zoom < 1.75 || pick.r < 10 || labels.length >= 14)) continue;
    const name = String(pick.doc.label || 'Без названия');
    const text = name.length > 42 ? name.slice(0, 28) + '…' + name.slice(-12) : name;
    const labelWidth = Math.min(width - 16, 258, Math.ceil(ctx.measureText(text).width) + 18);
    const left = clamp(pick.x - labelWidth / 2, 8, width - labelWidth - 8);
    const top = pick.top - 30;
    const bounds = {left:left-4,right:left+labelWidth+4,top:top-3,bottom:top+27};
    if (!visible(pick,width,height,0) || top < 134 || bounds.bottom > height - 100
        || occupied.some(box => overlaps(box, bounds))) continue;
    occupied.push(bounds);
    labels.push({doc:pick.doc, left, right:left+labelWidth, top, bottom:top+24});
    ctx.beginPath();
    ctx.roundRect(left, top, labelWidth, 24, 8);
    ctx.fillStyle = active ? '#214e38' : '#fafbf6';
    ctx.fill();
    ctx.strokeStyle = active ? '#214e38' : '#bbcdb0';
    ctx.stroke();
    ctx.fillStyle = active ? '#fff' : '#143b2a';
    ctx.fillText(text, left + labelWidth / 2, top + 12, labelWidth - 14);
  }
  return labels;
}

export function createForestScene(canvas, controls, onDocument, onGrove, onOverview) {
  const ctx = canvas.getContext('2d');
  let camera = {...initialCamera}, layout = groveLayout({datasets:[], documents:[]}, '');
  let selected = '', hovered = '', frame = 0, pointer = null;
  let treePicks = [], grovePicks = [], documentLabels = [], catalogRef, datasetRef;

  function schedule() {
    if (ctx && !frame) frame = requestAnimationFrame(() => { frame = 0; draw(); });
  }

  function zoom(factor) {
    camera.zoom = clamp(camera.zoom * factor, .65, 4);
    controls.scale.textContent = `${Math.round(camera.zoom * 100)}%`;
    controls.minus.disabled = camera.zoom <= .65;
    controls.plus.disabled = camera.zoom >= 4;
    schedule();
  }

  function reset() {
    camera = {...initialCamera};
    hovered = '';
    zoom(1);
  }

  function draw() {
    const {width:w, height:h} = canvas.getBoundingClientRect();
    if (w < 30 || h < 270) return;
    const dpr = Math.min(globalThis.devicePixelRatio || 1, 2);
    if (canvas.width !== Math.round(w*dpr)) canvas.width = Math.round(w*dpr);
    if (canvas.height !== Math.round(h*dpr)) canvas.height = Math.round(h*dpr);
    ctx.setTransform(dpr,0,0,dpr,0,0);
    ctx.clearRect(0,0,w,h);
    const project = projection(camera, w, h, layout.extent);
    treePicks = [];
    grovePicks = [];
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 132, w, h - 188);
    ctx.clip();
    const groves = layout.groves.map(grove => ({...grove, center:project(grove.x, 0, grove.z)}))
      .filter(grove => visible(grove.center, w, h, 240)).sort((a, b) => a.center.depth - b.center.depth);
    const trees = [], labels = [];
    const treeLimit = Math.max(1, Math.floor(maximumTrees / Math.max(groves.length, 1)));
    for (const grove of groves) {
      const edge = project(grove.x + radius, 0, grove.z);
      const small = Math.hypot(edge.x - grove.center.x, edge.y - grove.center.y) < 17;
      const border = drawGrove(ctx, project, grove, small ? 8 : 24);
      grovePicks.push({grove, border});
      if (small) continue;
      const horizontal = Math.max(...border.map(p => p.x)) - Math.min(...border.map(p => p.x));
      const allowed = Math.max(1, Math.min(treeLimit, Math.floor(horizontal ** 2 / 350)));
      const entries = grove.visibleEntries;
      const stride = Math.max(1, Math.ceil(entries.length / allowed));
      const density = clamp(10 / Math.sqrt(Math.max(grove.count, 1)), .43, .91);
      const addTree = ({doc, position, species}) => {
        const x = grove.x + position.x * radius * .87, z = grove.z + position.z * radius * .87;
        const center = project(x, 0, z);
        if (visible(center, w, h, 32)) trees.push({doc, species, x, z, depth:center.depth, height:position.height*density});
      };
      for (let sample = 0; sample < entries.length; sample += stride) {
        addTree(entries[sample]);
      }
      if (grove.selectionIndex >= 0 && grove.selectionIndex % stride) addTree(entries[grove.selectionIndex]);
      if (horizontal >= 48) labels.push({grove, horizontal, y:Math.max(...border.map(p => p.y)) + 13});
    }
    trees.sort((a, b) => a.depth - b.depth);
    for (const tree of trees) {
      const chosen = tree.doc.id === selected;
      treePicks.push(drawTree(ctx, project, tree, chosen, camera.zoom > 1.15 || groves.length <= 3));
    }
    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    const groveLabels = [];
    for (const {grove, horizontal, y} of labels) {
      ctx.font = '600 14px "Segoe UI",sans-serif';
      const title = String(grove.label || 'Роща');
      const text = title.length > 30 ? title.slice(0, 29) + '…' : title;
      const width = Math.min(horizontal + 20, Math.max(108, ctx.measureText(text).width + 16));
      const x = grove.center.x - width / 2;
      if (y < 136 || y + 33 > h - 56) continue;
      ctx.fillStyle = '#f5f7ed';
      ctx.beginPath();
      ctx.roundRect(x, y - 4, width, 37, 8);
      ctx.fill();
      ctx.fillStyle = '#143b2a';
      ctx.fillText(text, grove.center.x, y, width - 12);
      ctx.fillStyle = '#536656';
      ctx.font = '12px "Segoe UI",sans-serif';
      ctx.fillText(counted(grove.count,'документ','документа','документов'), grove.center.x, y + 17, width - 12);
      grovePicks.push({grove, border:[{x,y:y-4},{x:x+width,y:y-4},{x:x+width,y:y+33},{x,y:y+33}], label:true});
      groveLabels.push({left:x, right:x+width, top:y-4, bottom:y+33});
    }
    documentLabels = drawDocumentLabels(ctx, treePicks, groveLabels, selected, hovered, camera.zoom, w, h);
    ctx.restore();
  }

  function hit(event) {
    const rect = canvas.getBoundingClientRect(), x = event.clientX - rect.left, y = event.clientY - rect.top;
    if (y < 132 || y > rect.height - 56) return undefined;
    return documentLabels.findLast(label => x >= label.left && x <= label.right && y >= label.top && y <= label.bottom)
      || grovePicks.findLast(p => p.label && contains(p.border, x, y))
      || treePicks.findLast(p => Math.hypot(p.x - x, p.y - y) < p.r)
      || grovePicks.findLast(p => contains(p.border, x, y));
  }

  canvas.onpointerdown = event => {
    if (event.button !== 0 || pointer) return;
    pointer = {id:event.pointerId, x:event.clientX, y:event.clientY, yaw:camera.yaw, pitch:camera.pitch, drag:false};
    canvas.setPointerCapture?.(event.pointerId);
  };
  function hover(documentId) {
    if (documentId === hovered) return;
    hovered = documentId;
    schedule();
  }
  canvas.onpointermove = event => {
    if (!pointer) {
      const target = hit(event);
      canvas.style.cursor = target ? 'pointer' : 'grab';
      hover(camera.zoom > 1 ? target?.doc?.id || '' : '');
      return;
    }
    if (event.pointerId !== pointer.id) return;
    const dx = event.clientX - pointer.x, dy = event.clientY - pointer.y;
    if (Math.hypot(dx, dy) >= 5) pointer.drag = true;
    if (!pointer.drag) return;
    canvas.style.cursor = 'grabbing';
    hovered = '';
    camera.yaw = (pointer.yaw + dx * .008) % (Math.PI * 2);
    camera.pitch = clamp(pointer.pitch + dy * .006, .23, 1.43);
    schedule();
  };
  canvas.onpointerleave = () => hover('');
  canvas.onpointerup = event => {
    if (!pointer || event.pointerId !== pointer.id) return;
    const click = !pointer.drag;
    pointer = null;
    canvas.style.cursor = 'grab';
    if (!click) return;
    const target = hit(event);
    if (target?.doc) onDocument(target.doc);
    else if (target?.grove) onGrove(target.grove.id);
  };
  canvas.onpointercancel = canvas.onlostpointercapture = () => {
    pointer = null;
    canvas.style.cursor = 'grab';
  };
  canvas.addEventListener('wheel', event => {
    if (!layout.groves.length || event.ctrlKey) return;
    event.preventDefault();
    zoom(event.deltaY < 0 ? 1.12 : 1 / 1.12);
  }, {passive:false});
  canvas.onkeydown = event => {
    const actions = {
      ArrowLeft: () => { camera.yaw -= .16; }, ArrowRight: () => { camera.yaw += .16; },
      ArrowUp: () => { camera.pitch = clamp(camera.pitch - .12, .23, 1.43); },
      ArrowDown: () => { camera.pitch = clamp(camera.pitch + .12, .23, 1.43); },
      '+': () => zoom(1.25), '=': () => zoom(1.25), '-': () => zoom(.8),
      Home: onOverview, Escape: onOverview,
    };
    if (!Object.hasOwn(actions,event.key)) return;
    event.preventDefault();
    actions[event.key]();
    schedule();
  };
  controls.plus.onclick = () => zoom(1.25);
  controls.minus.onclick = () => zoom(.8);
  controls.overview.onclick = onOverview;
  new ResizeObserver(schedule).observe(canvas);
  return {
    reset,
    update(catalog, dataset, documents, selection, isFiltered) {
      if (catalog !== catalogRef || dataset !== datasetRef) {
        layout = groveLayout(catalog, dataset);
        catalogRef = catalog;
        datasetRef = dataset;
        reset();
      }
      selected = selection;
      const ids = isFiltered ? new Set(documents.map(doc => doc.id)) : null;
      for (const grove of layout.groves) {
        grove.visibleEntries = ids ? grove.entries.filter(({doc}) => ids.has(doc.id)) : grove.entries;
        grove.selectionIndex = selected ? grove.visibleEntries.findIndex(({doc}) => doc.id === selected) : -1;
      }
      schedule();
    },
  };
}

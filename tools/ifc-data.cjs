// Read saved IFC metadata using the same WebIFC decoder as the viewer.
'use strict';
const fs = require('node:fs');
const ifc = require('web-ifc');

const MAX_OBJECTS = 20_000;
const MAX_OUTPUT = 128 * 1024 * 1024;

function scalar(field) {
  const value = field?.value;
  return typeof value === 'string' || typeof value === 'boolean' || Number.isFinite(value)
    ? value : null;
}

function refs(value) {
  if (Array.isArray(value)) return value.flatMap(refs);
  if (Array.isArray(value?.value)) return value.value.flatMap(refs);
  return value?.type === 5 && Number.isSafeInteger(value.value) && value.value > 0 ? [value.value] : [];
}

function* entityIds(api, model, type) {
  const ids = api.GetLineIDsWithType(model, type, true);
  for (let i = 0; i < ids.size(); i++) yield ids.get(i);
}

function projection(api, model) {
  const objects = new Map();
  const propertySets = new Map();
  const line = id => api.GetLine(model, id, false, false);
  const className = id => api.GetNameFromTypeCode(api.GetLineType(model, id));
  const units = new Map();

  function unit(id) {
    if (units.has(id)) return units.get(id);
    const item = line(id);
    const saved = {
      step_id: id,
      class: className(id),
      unit_type: scalar(item.UnitType),
      prefix: scalar(item.Prefix),
      name: scalar(item.Name),
    };
    units.set(id, saved);
    return saved;
  }
  const projectUnits = new Map();
  const unitByType = new Map();
  for (const id of entityIds(api, model, ifc.IFCPROJECT)) {
    const assignments = refs(line(id).UnitsInContext);
    const assignedUnits = assignments.flatMap(assignment => refs(line(assignment).Units)).map(unit);
    projectUnits.set(id, assignedUnits);
    for (const saved of assignedUnits) {
      if (!saved.unit_type) continue;
      const previous = unitByType.get(saved.unit_type);
      unitByType.set(saved.unit_type, previous === undefined || previous === saved ? saved : null);
    }
  }

  function definition(id) {
    if (propertySets.has(id)) return propertySets.get(id);
    const item = line(id);
    const properties = [];
    for (const propertyId of refs(item.HasProperties ?? item.Quantities)) {
      const row = line(propertyId);
      const quantityField = ['LengthValue', 'AreaValue', 'VolumeValue', 'CountValue', 'WeightValue',
        'TimeValue', 'NumberValue'].find(key => key in row);
      const field = row.NominalValue ?? row.ListValues ?? row.EnumerationValues ?? row[quantityField];
      const value = Array.isArray(field) ? field.map(scalar) : scalar(field);
      if (value === null && row.NominalValue === undefined) continue;
      const enumeration = refs(row.EnumerationReference)[0];
      const explicitUnit = refs(row.Unit ?? (enumeration ? line(enumeration).Unit : null))[0];
      const dimension = field?.name?.match(/^IFC(?:POSITIVE|NONNEGATIVE)?([A-Z]+)MEASURE$/)?.[1]
        ?? ({ LengthValue: 'LENGTH', AreaValue: 'AREA', VolumeValue: 'VOLUME',
          WeightValue: 'MASS', TimeValue: 'TIME' })[quantityField];
      const savedUnit = explicitUnit ? unit(explicitUnit)
        : projectUnits.size === 1 ? unitByType.get(`${dimension}UNIT`) : null;
      const name = scalar(row.Name) ?? className(propertyId);
      const qualifiers = [field?.name, savedUnit && [savedUnit.prefix, savedUnit.name].filter(Boolean).join(' ')]
        .filter(Boolean).join(', ');
      const literal = value === null ? 'значение не задано' : Array.isArray(value) ? JSON.stringify(value) : String(value);
      const unitSource = savedUnit ? `, ${explicitUnit ? 'единица' : 'единица проекта'} #${savedUnit.step_id}` : '';
      properties.push(`${name}: ${literal}${qualifiers ? ` [${qualifiers}]` : ''} (#${propertyId}${unitSource})`);
    }
    const set = { step_id: id, name: scalar(item.Name), properties };
    propertySets.set(id, set);
    return set;
  }

  for (const [type, section] of [
    [ifc.IFCPRODUCT, 'elements'], [ifc.IFCTYPEOBJECT, 'types'],
    [ifc.IFCSYSTEM, 'systems'], [ifc.IFCPROJECT, 'project'],
  ]) {
    for (const id of entityIds(api, model, type)) {
      const category = className(id);
      if (category === 'IfcDistributionPort') continue;
      const item = line(id);
      const name = scalar(item.Name) || scalar(item.LongName);
      const properties = {};
      for (const key of ['Name', 'LongName', 'Description', 'ObjectType', 'ElementType', 'PredefinedType', 'Tag', 'Elevation', 'Phase']) {
        const value = scalar(item[key]);
        if (value !== null && value !== '') properties[key] = value;
      }
      const kind = category === 'IfcBuildingStorey' ? 'levels' : section;
      objects.set(id, {
        object_id: `${kind}:${id}`, section: kind, native_id: `#${id}`, name, category, properties,
        identifiers: { step_id: id, global_id: scalar(item.GlobalId) },
        value_semantics: 'saved_ifc', model_context: {},
      });
      for (const setId of refs(item.HasPropertySets)) {
        (properties['Наборы свойств'] ??= {})[setId] = definition(setId);
      }
      if (objects.size > MAX_OBJECTS) throw new Error('IFC_OBJECT_LIMIT');
    }
  }

  const parents = new Map();
  for (const [type, childKey, parentKey] of [
    [ifc.IFCRELCONTAINEDINSPATIALSTRUCTURE, 'RelatedElements', 'RelatingStructure'],
    [ifc.IFCRELAGGREGATES, 'RelatedObjects', 'RelatingObject'],
    [ifc.IFCRELNESTS, 'RelatedObjects', 'RelatingObject'],
  ]) {
    for (const id of entityIds(api, model, type)) {
      const relation = line(id);
      for (const child of refs(relation[childKey])) {
        if (!objects.has(child)) continue;
        if (!parents.has(child)) parents.set(child, []);
        for (const target of refs(relation[parentKey])) parents.get(child).push({ target, relation: id });
      }
    }
  }

  function attach(object, key, target, via) {
    const related = objects.get(target);
    if (!related || object === related) return;
    const context = object.model_context[key] ??= [];
    if (!context.some(entry => entry.native_id === related.native_id)) {
      context.push({ native_id: related.native_id, name: related.name, via: via.map(id => `#${id}`) });
    }
  }

  for (const [id, object] of objects) {
    const seen = new Set([id]);
    const queue = [[id, []]];
    for (let i = 0; i < queue.length; i++) {
      const [current, path] = queue[i];
      for (const { target, relation } of parents.get(current) ?? []) {
        if (seen.has(target)) continue;
        seen.add(target);
        const via = [...path, relation];
        const parent = objects.get(target);
        if (parent?.section === 'levels') attach(object, 'levels', target, via);
        else if (parent?.category === 'IfcSpace') attach(object, 'spaces', target, via);
        if (parent && parent.section !== 'levels' && via.length < 32) queue.push([target, via]);
      }
    }
  }

  for (const [type, key, relationKey] of [
    [ifc.IFCRELDEFINESBYTYPE, 'types', 'RelatingType'],
    [ifc.IFCRELASSIGNSTOGROUP, 'systems', 'RelatingGroup'],
    [ifc.IFCRELDEFINESBYPROPERTIES, 'properties', 'RelatingPropertyDefinition'],
  ]) {
    for (const id of entityIds(api, model, type)) {
      const relation = line(id);
      const targets = refs(relation[relationKey]);
      for (const child of refs(relation.RelatedObjects)) {
        const object = objects.get(child);
        if (!object) continue;
        for (const target of targets) {
          if (key === 'properties') {
            (object.properties['Наборы свойств'] ??= {})[target] = { ...definition(target), relation_id: id };
          } else if (objects.get(target)?.section === key) {
            attach(object, key, target, [id]);
          }
        }
      }
    }
  }
  for (const [id, object] of objects) {
    if (object.section !== 'project') continue;
    object.properties['Единицы проекта'] = projectUnits.get(id).map(saved =>
      [saved.unit_type ?? saved.class, saved.prefix, saved.name, `(#${saved.step_id})`].filter(Boolean).join(' '));
  }
  return { objects: [...objects.values()], unit_count: new Set([...projectUnits.values()].flat()).size };
}

async function main(source, output) {
  const api = new ifc.IfcAPI();
  await api.Init();
  const model = api.OpenModel(fs.readFileSync(source), { MEMORY_LIMIT: 512 * 1024 * 1024 });
  if (model < 0) throw new Error('IFC_OPEN_FAILED');
  try {
    const result = projection(api, model);
    const json = JSON.stringify({ schema: 'les.ifc-data.v1', ifc_schema: api.GetModelSchema(model), ...result });
    if (Buffer.byteLength(json) > MAX_OUTPUT) throw new Error('IFC_OUTPUT_LIMIT');
    fs.writeFileSync(output, json, { flag: 'wx' });
  } finally {
    api.CloseModel(model);
  }
}

main(process.argv[2], process.argv[3]).catch(error => {
  console.error(error.message);
  process.exitCode = error.message.endsWith('_LIMIT') ? 2 : 1;
});

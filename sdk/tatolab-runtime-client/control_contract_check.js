// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import { CONTROL_CONTRACT_SCHEMAS } from "./control_contract_schemas.js";

const COMPONENT_SCHEMA_REFERENCE_PREFIX = "#/components/schemas/";
const READS_ITS_DEFAULT_MARK = "x-generated-reads-its-default";
const UUID_PATTERN = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const ARRAY_KEYWORDS = ["items", "minItems", "maxItems"];
const OBJECT_KEYWORDS = ["properties", "required", "additionalProperties"];
const WIDE_INTEGER_FORMATS = new Set(["int64", "uint64"]);
const INTEGER_RANGE_BY_FORMAT = new Map([
  ["int8", [-(2n ** 7n), 2n ** 7n - 1n]],
  ["int16", [-(2n ** 15n), 2n ** 15n - 1n]],
  ["int32", [-(2n ** 31n), 2n ** 31n - 1n]],
  ["int64", [-(2n ** 63n), 2n ** 63n - 1n]],
  ["uint8", [0n, 2n ** 8n - 1n]],
  ["uint16", [0n, 2n ** 16n - 1n]],
  ["uint32", [0n, 2n ** 32n - 1n]],
  ["uint64", [0n, 2n ** 64n - 1n]],
  ["uint", [0n, undefined]],
]);

function jsonTypeOf(value) {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  if (typeof value === "bigint") return "integer";
  if (typeof value === "number") return Number.isInteger(value) ? "integer" : "number";
  return typeof value;
}

function isAJsonObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sameJson(left, right) {
  if (left === right) return true;
  if (typeof left === "bigint" || typeof right === "bigint") {
    return (typeof left === "bigint" || typeof left === "number") && (typeof right === "bigint" || typeof right === "number") && left == right;
  }
  if (Array.isArray(left)) {
    return Array.isArray(right) && left.length === right.length && left.every((item, index) => sameJson(item, right[index]));
  }
  if (isAJsonObject(left) && isAJsonObject(right)) {
    const leftKeys = Object.keys(left);
    return leftKeys.length === Object.keys(right).length && leftKeys.every((key) => Object.hasOwn(right, key) && sameJson(left[key], right[key]));
  }
  return false;
}

function describedValue(value) {
  return typeof value === "bigint" ? `${value}` : JSON.stringify(value);
}

function componentSchemaNamed(reference) {
  const name = reference.startsWith(COMPONENT_SCHEMA_REFERENCE_PREFIX) ? reference.slice(COMPONENT_SCHEMA_REFERENCE_PREFIX.length) : reference;
  return Object.hasOwn(CONTROL_CONTRACT_SCHEMAS.components, name) ? CONTROL_CONTRACT_SCHEMAS.components[name] : undefined;
}

function theAlternativesOf(schema) {
  return schema.oneOf ?? schema.anyOf;
}

function thePropertyNamesOf(schema) {
  return isAJsonObject(schema?.properties) ? Object.keys(schema.properties) : [];
}

/** One walk of a value against one schema: the issues it finds, and the value as read. */
class ControlContractReader {
  constructor(direction) {
    this.direction = direction;
    this.issues = [];
  }

  issue(code, path, message) {
    this.issues.push({ code, path, message });
  }

  /** Reads `value` against `schema` at `path` and answers it as read: the same value unless a default or a fallback was read into it. `alsoKnownKeys` names the keys a schema beside this one states. */
  read(schema, value, path, alsoKnownKeys = []) {
    if (schema === true) return value;
    if (schema === false) {
      this.issue("not_allowed", path, "no value is allowed here");
      return value;
    }
    let read = value;
    if (typeof schema.$ref === "string") {
      const named = componentSchemaNamed(schema.$ref);
      if (named === undefined) {
        this.issue("unknown_reference", path, `the contract has no component \`${schema.$ref}\``);
        return value;
      }
      read = this.read(named, read, path, alsoKnownKeys);
    }
    if (schema["x-may-grow"] === true && this.direction === "read") {
      return this.readASetThatMayGrow(schema, read, path, alsoKnownKeys);
    }
    return this.readAgainstItsKeywords(schema, read, path, alsoKnownKeys);
  }

  /** A set marked `x-may-grow`, read: a value of its members' own JSON type it does not name reads as its last member, as the runtime reads one. */
  readASetThatMayGrow(schema, value, path, alsoKnownKeys) {
    const closedSetReader = new ControlContractReader(this.direction);
    const read = closedSetReader.readAgainstItsKeywords(schema, value, path, alsoKnownKeys);
    if (closedSetReader.issues.length === 0) return read;
    const alternatives = theAlternativesOf(schema) ?? [];
    const lastMember = alternatives.at(-1);
    if (lastMember !== undefined && Object.hasOwn(lastMember, "const") && jsonTypeOf(lastMember.const) === jsonTypeOf(value)) {
      return lastMember.const;
    }
    this.issues.push(...closedSetReader.issues);
    return value;
  }

  readAgainstItsKeywords(schema, value, path, alsoKnownKeys) {
    const issuesBefore = this.issues.length;
    if (Object.hasOwn(schema, "const") && !sameJson(value, schema.const)) {
      this.issue("invalid_value", path, `expected ${describedValue(schema.const)}, read ${describedValue(value)}`);
      return value;
    }
    if (schema.type !== undefined && !this.isOfAStatedType(schema, value, path)) {
      return value;
    }
    let read = value;
    if (typeof value === "number" || typeof value === "bigint") {
      this.checkANumber(schema, value, path);
    } else if (typeof value === "string") {
      this.checkAString(schema, value, path);
    } else if (Array.isArray(value) && ARRAY_KEYWORDS.some((keyword) => Object.hasOwn(schema, keyword))) {
      read = this.readAnArray(schema, value, path);
    } else if (isAJsonObject(value) && OBJECT_KEYWORDS.some((keyword) => Object.hasOwn(schema, keyword))) {
      read = this.readAnObject(schema, value, path, alsoKnownKeys);
    }
    const alternatives = theAlternativesOf(schema);
    if (alternatives !== undefined && this.issues.length === issuesBefore) {
      read = this.readOneAlternative(alternatives, read, path, [...alsoKnownKeys, ...thePropertyNamesOf(schema)]);
    }
    return read;
  }

  isOfAStatedType(schema, value, path) {
    const statedTypes = Array.isArray(schema.type) ? schema.type : [schema.type];
    const valueType = jsonTypeOf(value);
    const isOfOne = statedTypes.some((statedType) => {
      if (statedType === valueType) {
        return typeof value !== "bigint" || WIDE_INTEGER_FORMATS.has(schema.format);
      }
      return statedType === "number" && valueType === "integer" && typeof value === "number";
    });
    if (!isOfOne) {
      this.issue("invalid_type", path, `expected ${statedTypes.join(" or ")}, read ${valueType === "integer" && typeof value === "bigint" ? "an integer past 2^53" : valueType}`);
    }
    return isOfOne;
  }

  checkANumber(schema, value, path) {
    if (typeof value === "number" && !Number.isFinite(value)) {
      this.issue("invalid_type", path, "expected a finite number");
      return;
    }
    if (typeof schema.minimum === "number" && value < schema.minimum) {
      this.issue("too_small", path, `expected at least ${schema.minimum}, read ${value}`);
    }
    const range = INTEGER_RANGE_BY_FORMAT.get(schema.format);
    if (range !== undefined && (typeof value === "bigint" || Number.isInteger(value))) {
      const [lowest, highest] = range;
      const whole = BigInt(value);
      if (whole < lowest || (highest !== undefined && whole > highest)) {
        this.issue("out_of_range", path, `${value} is outside the range of a ${schema.format}`);
      }
    }
  }

  checkAString(schema, value, path) {
    if (schema.format === "uuid" && !UUID_PATTERN.test(value)) {
      this.issue("invalid_format", path, `expected a UUID, read ${describedValue(value)}`);
    }
  }

  readAnArray(schema, value, path) {
    if (typeof schema.minItems === "number" && value.length < schema.minItems) {
      this.issue("too_small", path, `expected at least ${schema.minItems} items, read ${value.length}`);
    }
    if (typeof schema.maxItems === "number" && value.length > schema.maxItems) {
      this.issue("too_big", path, `expected at most ${schema.maxItems} items, read ${value.length}`);
    }
    if (schema.items === undefined) return value;
    let read = value;
    value.forEach((item, index) => {
      const itemRead = this.read(schema.items, item, [...path, index]);
      if (itemRead !== item) {
        read = read === value ? [...value] : read;
        read[index] = itemRead;
      }
    });
    return read;
  }

  readAnObject(schema, value, path, alsoKnownKeys) {
    const properties = isAJsonObject(schema.properties) ? schema.properties : {};
    let read = value;
    const readInto = (key, propertyRead) => {
      read = read === value ? { ...value } : read;
      read[key] = propertyRead;
    };
    for (const [key, propertySchema] of Object.entries(properties)) {
      if (Object.hasOwn(value, key)) {
        const propertyRead = this.read(propertySchema, value[key], [...path, key]);
        if (propertyRead !== value[key]) readInto(key, propertyRead);
      } else if (this.direction === "read" && propertySchema?.[READS_ITS_DEFAULT_MARK] === true) {
        readInto(key, propertySchema.default);
      } else if (Array.isArray(schema.required) && schema.required.includes(key)) {
        this.issue("missing", [...path, key], "required, and absent");
      }
    }
    for (const key of Array.isArray(schema.required) ? schema.required : []) {
      if (!Object.hasOwn(properties, key) && !Object.hasOwn(value, key)) {
        this.issue("missing", [...path, key], "required, and absent");
      }
    }
    for (const key of Object.keys(value)) {
      if (Object.hasOwn(properties, key)) continue;
      const additionalProperties = schema.additionalProperties;
      if (additionalProperties !== undefined && additionalProperties !== false) {
        const propertyRead = this.read(additionalProperties, value[key], [...path, key]);
        if (propertyRead !== value[key]) readInto(key, propertyRead);
      } else if (this.direction === "sent" && theAlternativesOf(schema) === undefined && !alsoKnownKeys.includes(key)) {
        this.issue("unknown_key", [...path, key], "the contract names no such field");
      }
    }
    return read;
  }

  /** The first alternative `value` reads as, or the issues of the one its tag names. */
  readOneAlternative(alternatives, value, path, alsoKnownKeys) {
    let issuesOfTheTaggedAlternative;
    for (const alternative of alternatives) {
      const alternativeReader = new ControlContractReader(this.direction);
      const read = alternativeReader.read(alternative, value, path, alsoKnownKeys);
      if (alternativeReader.issues.length === 0) return read;
      if (issuesOfTheTaggedAlternative === undefined && this.isTheAlternativeItsTagNames(alternative, value)) {
        issuesOfTheTaggedAlternative = alternativeReader.issues;
      }
    }
    if (issuesOfTheTaggedAlternative !== undefined) {
      this.issues.push(...issuesOfTheTaggedAlternative);
    } else {
      this.issue("no_alternative", path, `matches none of the ${alternatives.length} alternatives the contract allows here`);
    }
    return value;
  }

  isTheAlternativeItsTagNames(alternative, value) {
    const resolved = typeof alternative?.$ref === "string" ? componentSchemaNamed(alternative.$ref) : alternative;
    if (!isAJsonObject(value) || !isAJsonObject(resolved?.properties)) return false;
    const tags = Object.entries(resolved.properties).filter(([, propertySchema]) => isAJsonObject(propertySchema) && Object.hasOwn(propertySchema, "const"));
    return tags.length > 0 && tags.every(([key, propertySchema]) => sameJson(value[key], propertySchema.const));
  }
}

export function readAgainstControlContractSchema(schema, value, direction) {
  if (direction !== "sent" && direction !== "read") {
    throw new TypeError(`a value is checked as \`sent\` or \`read\`, not as ${describedValue(direction)}`);
  }
  const reader = new ControlContractReader(direction);
  const read = reader.read(schema, value, []);
  return { issues: reader.issues, value: read };
}

function theComponentSchemaNamed(name) {
  if (!Object.hasOwn(CONTROL_CONTRACT_SCHEMAS.components, name)) {
    throw new TypeError(`the control contract has no component named ${describedValue(name)}`);
  }
  return CONTROL_CONTRACT_SCHEMAS.components[name];
}

export function controlContractIssues(name, value, direction) {
  return readAgainstControlContractSchema(theComponentSchemaNamed(name), value, direction).issues;
}

export function readControlContractValue(name, value) {
  const { issues, value: read } = readAgainstControlContractSchema(theComponentSchemaNamed(name), value, "read");
  return issues.length === 0 ? { read: true, value: read } : { read: false, issues };
}

export function isControlContractValue(name, value) {
  const { issues, value: read } = readAgainstControlContractSchema(theComponentSchemaNamed(name), value, "read");
  return issues.length === 0 && read === value;
}

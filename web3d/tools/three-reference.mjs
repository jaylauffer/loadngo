#!/usr/bin/env node
// Reference radiance values from three.js r180's own shading formulas, for web3d's lighting tests.
//
// web3d shades as three.js r180's MeshPhongMaterial does under a DirectionalLight and a
// HemisphereLight (physically based light units: diffuse is albedo / pi times irradiance), with
// colour = base * (1 - metallic), specular colour = mix(0.04, base, metallic), and a shininess
// from roughness by web3d's own mapping (below; three has no such mapping). This script
// transcribes those formulas from three's shader chunks, first checking that every transcribed
// line is in three's source verbatim, so a change in three or a slip here fails loudly. It writes
// tools/three-reference.txt, which the Rust unit test (shaders.rs) and the GPU probe
// (demo/probe.js) both check against.
//
//   THREE_DIR=/path/to/node_modules/three node web3d/tools/three-reference.mjs           # write
//   THREE_DIR=/path/to/node_modules/three node web3d/tools/three-reference.mjs --check   # compare, exit 1 if stale
//
// BSD-3-Clause; see the repository's LICENSE.

import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const three = process.env.THREE_DIR;
if (!three) {
  console.error('set THREE_DIR to a three.js r180 package (node_modules/three)');
  process.exit(2);
}
const version = JSON.parse(readFileSync(path.join(three, 'package.json'), 'utf8')).version;
const chunk = (name) => readFileSync(path.join(three, 'src/renderers/shaders/ShaderChunk', `${name}.glsl.js`), 'utf8');
const source = (rel) => readFileSync(path.join(three, rel), 'utf8');

// What is transcribed below, and where three has it.
const LINES = [
  ['common', 'return RECIPROCAL_PI * diffuseColor;'], // BRDF_Lambert
  ['common', 'float fresnel = exp2( ( - 5.55473 * dotVH - 6.98316 ) * dotVH );'], // F_Schlick
  ['common', 'return f0 * ( 1.0 - fresnel ) + ( f90 * fresnel );'],
  ['bsdfs', 'return 0.25;'], // G_BlinnPhong_Implicit
  ['bsdfs', 'return RECIPROCAL_PI * ( shininess * 0.5 + 1.0 ) * pow( dotNH, shininess );'], // D_BlinnPhong
  ['bsdfs', 'vec3 halfDir = normalize( lightDir + viewDir );'],
  ['bsdfs', 'float dotNH = saturate( dot( normal, halfDir ) );'],
  ['bsdfs', 'float dotVH = saturate( dot( viewDir, halfDir ) );'],
  ['bsdfs', 'vec3 F = F_Schlick( specularColor, 1.0, dotVH );'],
  ['bsdfs', 'return F * ( G * D );'],
  ['lights_phong_pars_fragment', 'float dotNL = saturate( dot( geometryNormal, directLight.direction ) );'],
  ['lights_phong_pars_fragment', 'vec3 irradiance = dotNL * directLight.color;'],
  ['lights_phong_pars_fragment', 'reflectedLight.directDiffuse += irradiance * BRDF_Lambert( material.diffuseColor );'],
  ['lights_phong_pars_fragment', 'reflectedLight.directSpecular += irradiance * BRDF_BlinnPhong( directLight.direction, geometryViewDir, geometryNormal, material.specularColor, material.specularShininess ) * material.specularStrength;'],
  ['lights_phong_pars_fragment', 'reflectedLight.indirectDiffuse += irradiance * BRDF_Lambert( material.diffuseColor );'],
  ['lights_pars_begin', 'float hemiDiffuseWeight = 0.5 * dotNL + 0.5;'],
  ['lights_pars_begin', 'vec3 irradiance = mix( hemiLight.groundColor, hemiLight.skyColor, hemiDiffuseWeight );'],
];
for (const [name, line] of LINES) {
  if (!chunk(name).includes(line)) throw new Error(`three ${version}: ${name}.glsl.js no longer has: ${line}`);
}
// Light units: colour times intensity, with no factor of pi (r155+ physically based lights).
const lights = source('src/renderers/webgl/WebGLLights.js');
for (const line of ['uniforms.color.copy( light.color ).multiplyScalar( light.intensity );', 'uniforms.skyColor.copy( light.color ).multiplyScalar( intensity );', 'uniforms.groundColor.copy( light.groundColor ).multiplyScalar( intensity );']) {
  if (!lights.includes(line)) throw new Error(`three ${version}: WebGLLights.js no longer has: ${line}`);
}

const RECIPROCAL_PI = 1 / Math.PI;
const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const add = (a, b) => a.map((v, i) => v + b[i]);
const mul = (a, b) => a.map((v, i) => v * b[i]);
const scale = (a, s) => a.map((v) => v * s);
const norm = (a) => scale(a, 1 / Math.hypot(...a));
const saturate = (x) => Math.min(1, Math.max(0, x));
const mix = (a, b, t) => a.map((v, i) => v * (1 - t) + b[i] * t);

const BRDF_Lambert = (diffuseColor) => scale(diffuseColor, RECIPROCAL_PI);
const F_Schlick = (f0, f90, dotVH) => {
  const fresnel = 2 ** ((-5.55473 * dotVH - 6.98316) * dotVH);
  return f0.map((v) => v * (1 - fresnel) + f90 * fresnel);
};
const G_BlinnPhong_Implicit = () => 0.25;
const D_BlinnPhong = (shininess, dotNH) => RECIPROCAL_PI * (shininess * 0.5 + 1) * dotNH ** shininess;
const BRDF_BlinnPhong = (lightDir, viewDir, normal, specularColor, shininess) => {
  const halfDir = norm(add(lightDir, viewDir));
  const dotNH = saturate(dot(normal, halfDir));
  const dotVH = saturate(dot(viewDir, halfDir));
  const F = F_Schlick(specularColor, 1, dotVH);
  return scale(F, G_BlinnPhong_Implicit() * D_BlinnPhong(shininess, dotNH));
};
const getHemisphereLightIrradiance = (sky, ground, up, normal) => {
  const dotNL = dot(normal, up);
  const hemiDiffuseWeight = 0.5 * dotNL + 0.5;
  return mix(ground, sky, hemiDiffuseWeight);
};

// web3d's own: a Blinn-Phong exponent from roughness, matched to GGX (alpha = r^2, n = 2/alpha^2 - 2),
// clamped to 1..2048 (shaders.rs, blinn_phong). Not three's.
const shininessOf = (roughness) => {
  const r = Math.max(Math.min(Math.max(roughness, 0), 1), 0.03);
  return Math.min(Math.max(2 / (r * r * r * r) - 2, 1), 2048);
};

/** three r180, MeshPhongMaterial (specularStrength 1), one DirectionalLight, one HemisphereLight (up = +Y). */
function three180(c) {
  const diffuseColor = scale(c.base, 1 - c.metallic);
  const specularColor = mix([0.04, 0.04, 0.04], c.base, c.metallic);
  const shininess = shininessOf(c.roughness);
  const normal = norm(c.normal);
  const viewDir = norm(c.view);
  const direction = norm(c.toward);
  // RE_Direct_BlinnPhong
  const dotNL = saturate(dot(normal, direction));
  const irradiance = scale(c.key, dotNL);
  const directDiffuse = mul(irradiance, BRDF_Lambert(diffuseColor));
  const directSpecular = mul(irradiance, BRDF_BlinnPhong(direction, viewDir, normal, specularColor, shininess));
  // RE_IndirectDiffuse_BlinnPhong, with the hemisphere light's irradiance
  const hemi = getHemisphereLightIrradiance(c.sky, c.ground, [0, 1, 0], normal);
  const indirectDiffuse = mul(hemi, BRDF_Lambert(diffuseColor));
  return add(add(directDiffuse, directSpecular), indirectDiffuse);
}

// The cases. The first four face the camera (normal and view +Z), as the GPU probe draws them.
const CASES = [
  { name: 'lambert-60deg', base: [0.8, 0.8, 0.8], metallic: 0, roughness: 1, normal: [0, 0, 1], toward: [0, Math.sqrt(3) / 2, 0.5], view: [0, 0, 1], key: [3, 3, 3], sky: [0, 0, 0], ground: [0, 0, 0] },
  { name: 'hemisphere-side', base: [0.5, 0.4, 0.3], metallic: 0, roughness: 1, normal: [0, 0, 1], toward: [0, 0, 1], view: [0, 0, 1], key: [0, 0, 0], sky: [2.5, 2.5, 2.5], ground: [0.5, 0.5, 0.5] },
  { name: 'glossy-highlight', base: [0.2, 0.3, 0.6], metallic: 0, roughness: 0.3, normal: [0, 0, 1], toward: [0, 0, 1], view: [0, 0, 1], key: [0.5, 0.5, 0.5], sky: [0, 0, 0], ground: [0, 0, 0] },
  { name: 'metal-key', base: [1, 0.77, 0.34], metallic: 1, roughness: 0.5, normal: [0, 0, 1], toward: [0, 0.6, 0.8], view: [0, 0, 1], key: [2, 2, 2], sky: [0, 0, 0], ground: [0, 0, 0] },
  // Oblique ones, for the CPU mirror only.
  { name: 'oblique-dielectric', base: [0.6, 0.2, 0.1], metallic: 0, roughness: 0.6, normal: [0.3, 0.8, 0.52], toward: [0.5, 1, 0.35], view: [-0.2, 0.4, 1], key: [5.3, 5.0, 4.5], sky: [0.9, 1.1, 1.5], ground: [0.3, 0.27, 0.22] },
  { name: 'grazing-fresnel', base: [0.9, 0.9, 0.9], metallic: 0, roughness: 0.2, normal: [0, 1, 0], toward: [0, 0.2, -1], view: [0, 0.2, 1], key: [4, 4, 4], sky: [1, 1, 1], ground: [0.2, 0.2, 0.2] },
  { name: 'half-metal-downward', base: [0.3, 0.6, 0.3], metallic: 0.5, roughness: 0.45, normal: [0.1, -0.9, 0.4], toward: [0.2, -0.5, 1], view: [0.3, -0.3, 1], key: [1.5, 1.5, 1.5], sky: [2, 2, 2], ground: [0.6, 0.4, 0.2] },
];

const fmt = (v) => v.map((x) => x.toPrecision(9)).join(' ');
const lines = [
  `# Reference radiance from three.js ${version}'s shading formulas (web3d/tools/three-reference.mjs; do not edit).`,
  '# name | base rgb, metallic, roughness | normal | toward light | toward viewer | key rgb (colour x intensity) | sky rgb | ground rgb | radiance rgb',
];
for (const c of CASES) {
  const out = three180(c);
  lines.push(`${c.name} ${fmt([...c.base, c.metallic, c.roughness, ...norm(c.normal), ...norm(c.toward), ...norm(c.view), ...c.key, ...c.sky, ...c.ground])} ${fmt(out)}`);
}
const file = path.join(path.dirname(fileURLToPath(import.meta.url)), 'three-reference.txt');
const text = `${lines.join('\n')}\n`;
console.log(`three ${version}: ${LINES.length + 3} transcribed lines found verbatim`);
if (process.argv.includes('--check')) {
  const same = readFileSync(file, 'utf8') === text;
  console.log(same ? `${file} matches three ${version} exactly` : `${file} is stale: rerun without --check`);
  process.exit(same ? 0 : 1);
}
writeFileSync(file, text);
console.log(`wrote ${file}`);
console.log(lines.slice(2).join('\n'));

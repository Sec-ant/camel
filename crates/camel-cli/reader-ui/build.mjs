import { build } from 'esbuild';
await build({entryPoints:['src.jsx'],bundle:true,minify:true,format:'iife',outfile:'../src/project/reader.js',define:{'process.env.NODE_ENV':'"production"'},loader:{'.woff2':'dataurl','.woff':'dataurl'}});

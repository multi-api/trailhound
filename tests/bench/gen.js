const fs = require('fs');
const words = ('casa perro gato ciudad montaña río búsqueda motor índice documento rápido memoria disco servidor cliente factura producto precio calidad entrega pedido usuario sistema datos seguro compresión texto palabra consulta resultado página enlace imagen video música libro autor editorial año mes día hora minuto mercado tienda oferta descuento envío garantía soporte técnico azul rojo verde negro blanco grande pequeño nuevo usado madera metal plástico vidrio algodón cuero digital manual automático eléctrico solar portátil').split(' ');
const cats = ['Electrónica','Hogar','Deportes','Libros','Ropa','Juguetes','Herramientas','Jardín'];
let seed = 42; const rnd = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296;
const pick = (a) => a[Math.floor(rnd() * a.length)];
const out = fs.createWriteStream(process.argv[2]);
const n = Number(process.argv[3]);
for (let i = 1; i <= n; i++) {
  const t = Array.from({length: 4}, () => pick(words)).join(' ');
  const d = Array.from({length: 25}, () => pick(words)).join(' ');
  out.write(JSON.stringify({id: i, title: t + ' ' + i, description: d, category: pick(cats), price: Math.round(rnd() * 100000) / 100, stock: Math.floor(rnd() * 500), created: 1700000000 + i}) + '\n');
}
out.end();

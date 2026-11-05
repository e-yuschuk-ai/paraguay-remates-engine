# SPEC — Paraguay Remates Engine

Partes principales a desarrollar para la **versión 1 (web)** de Paraguay Remates.

- **Fuente de verdad:** [`docs/Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf`](docs/Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf) (v3).
- **Racional:** [`docs/CONVERSATION.md`](docs/CONVERSATION.md).
- **Propuesta comercial:** [`docs/Paraguay_Remates_v2_EN.pdf`](docs/Paraguay_Remates_v2_EN.pdf) (v2).

Las referencias `RF-xx` (requisitos funcionales), `RT-xx` (invariantes del motor), `CA-xx`
(criterios de aceptación), `DEC-xx` (decisiones) y `P-xx` (pendientes) apuntan a la v3.

---

## 0. Visión general

Plataforma web de subastas agrícolas con vendedores verificados, para unos 12.000 usuarios.
Funciona en el navegador de cualquier celular o computadora y se integra con WhatsApp.
El backend se diseña para que una futura app móvil use los mismos servicios (DEC-01).

```
              ┌──────────────┐     ┌──────────────────────┐
  Navegador ──►  Web (UI)     ├────►  API de aplicación   │◄── (futura app móvil)
              └──────▲───────┘     └──────────┬───────────┘
                     │ WebSocket / polling     │
              ┌──────┴───────┐     ┌──────────▼───────────┐
              │ Tiempo real   │◄────┤  MOTOR DE SUBASTAS   │  ← única autoridad
              └──────────────┘     │ (pujas, cierres)      │
                                   └──────────┬───────────┘
                                              │ outbox de eventos / cola persistente
        ┌─────────────────┬───────────────────┼───────────────────┬────────────────┐
        ▼                 ▼                   ▼                   ▼                ▼
  Notificaciones   WhatsApp: captura   WhatsApp: canal    WhatsApp: pujas   Documentos y
  web              de mensajes         (publicaciones)    + confirmación    cobranza (PDF)
```

**Reglas transversales** (aplican a todas las partes):
1. El **motor** es la única autoridad para aceptar pujas y adjudicar, para todos los canales (§5.1).
2. Las **operaciones sensibles** verifican permisos en el servidor.
3. Los **conectores están aislados**: si falla uno, no se detienen las subastas ni los otros conectores (RF-25).
4. Todo cambio relevante queda en la **auditoría append-only** (RF-33).
5. Lo que sigue **pendiente** se implementa configurable, o se cierra con el cliente antes de aceptar el módulo.

---

## Parte 1 — Plataforma base e infraestructura

**Objetivo:** el esqueleto donde corre todo lo demás.

- Repositorio, entornos (desarrollo, staging, producción), CI y despliegue.
- Base de datos transaccional, migraciones y almacenamiento de archivos. Las fotos, los documentos de identidad y los PDF tienen permisos según su sensibilidad.
- API de aplicación con autenticación y autorización por rol, pensada para reutilizarse desde una app (§5.1, §5.4).
- Trabajos en segundo plano y una cola persistente con **patrón outbox**, para que ningún aviso se pierda entre la confirmación de una puja y su envío (§5.1).
- Auditoría append-only: actor, acción, motivo, referencia y fecha (§5.2).
- Observabilidad: latencia de aceptación de pujas, errores, conexiones activas, retraso de cierres, profundidad de colas y entregas fallidas, con alertas (§5.5).
- Backups verificados y prueba de restauración mensual (RF-33).
- Seguridad: cifrado en tránsito, límites de intentos y ningún dato sensible (documentos, códigos, máximos privados) en logs ni en eventos públicos (§5.5).

**Stack decidido:** backend en **Rust** y base **PostgreSQL 16**: una primaria al inicio, preparada para réplicas de lectura y backups con PITR. El backend no guarda estado, así que corren varias instancias en paralelo. Detalle en [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). Siguen abiertos la nube y el framework web.
**Pendientes:** P-06 (disponibilidad, RPO/RTO, retención de datos de identidad).

---

## Parte 2 — Identidad, usuarios y roles

**Requisitos:** RF-01 a RF-04 y RF-19 · **Aceptación:** CA-01 · **Decisiones:** DEC-04, DEC-12

- Registro con datos personales, celular y carga de cédula o RUC.
- Validación automática: formato, duplicados y dígito verificador del RUC. Los casos dudosos van a una cola de revisión manual. No hay biometría ni consulta a registros oficiales.
- Verificación del celular con código por **WhatsApp Business API oficial**.
- **2FA por WhatsApp** para poder pujar, con el dispositivo recordado 30 días.
- Modo lectura para quien no aporta identidad: puede explorar, pero no puede pujar ni publicar.
- Solicitud de vendedor: acepta el acuerdo de vendedor y un administrador la aprueba a mano.
- Aceptación de términos guardada con versión, fecha y hora. Los textos los redacta el cliente.
- Suspensión y baja de usuarios por el administrador.
- **Matriz de permisos:** visitante · usuario en lectura · postor habilitado · vendedor aprobado · administrador.

**A definir:** duración de los códigos, reintentos y recuperación de acceso (RF-02).

---

## Parte 3 — Publicaciones y catálogo

**Requisitos:** RF-05 a RF-08 · **Aceptación:** CA-02 · **Decisiones:** DEC-06, DEC-15

- La publica un vendedor aprobado, o el administrador en su nombre. Incluye:
  - ficha técnica y hasta 10 fotos;
  - modalidad y precio base (si corresponde);
  - moneda, ubicación e inspección.
- **Una moneda por publicación** (USD o PYG), sin conversión. Precisión y formato definidos por moneda (RF-06, RT-04).
- Incremento definido por el vendedor, con una **sugerencia por rango de precio** que puede editar. El cliente debe validar la tabla de rangos (RF-07).
- Vigencia configurable: por defecto 21 días visible y 7 días abierta a pujas. Inicio de visibilidad, apertura y cierre se guardan como fechas explícitas (RF-08).
- Moderación y baja por incumplimiento.
- Catálogo público con filtros por categoría, marca, año, ubicación y precio.
- Página de subasta legible de un vistazo: estado, oferta más alta, próxima oferta y tiempo restante.

---

## Parte 4 — Motor de subastas (núcleo)

**Requisitos:** RF-09 a RF-14 · **Invariantes:** RT-01 a RT-07 · **Aceptación:** CA-03 a CA-06 · **Decisiones:** DEC-05, DEC-08

> **Estado: implementado** en `crates/remates-engine` (dominio) y `crates/remates-store` (PostgreSQL).
> Falta: la anulación de ofertas, la baja de máximos y el ingreso por WhatsApp, que dependen de reglas pendientes (P-04, P-01).

Es la parte más crítica. Se diseña y se prueba primero como dominio puro, sin depender de la interfaz.

**Modalidades:**
- **Martillo:** sin base, referencia inicial cero; gana la mayor oferta válida.
- **Reserva:** el mínimo del vendedor es **público** y las ofertas empiezan ahí. Si no hay ofertas válidas, la subasta cierra desierta.

**Aceptación de pujas en el servidor:**
- Se valida identidad habilitada, subasta abierta, moneda, importe y escalón.
- La próxima oferta siempre es visible y el usuario confirma antes de pujar.
- El importe mostrado se revalida al confirmar.

**Puja máxima automática (proxy):**
- La ejecuta el servidor y sigue funcionando aunque el usuario se desconecte.
- Los empates se resuelven por orden de carga.

**Alargue:** 2 minutos si entra una puja válida en los últimos 2 minutos. Ambos valores son configurables.

**Trazabilidad:**
- Cada postor tiene un alias anónimo por publicación.
- El historial guarda usuario real, importe y marca temporal.
- El administrador puede anular una oferta dejando el motivo.

**Cierre automático por reloj:** termina adjudicada o desierta, con como máximo **una** adjudicación vigente.

**Invariantes obligatorios:**
- **RT-01 Atomicidad por subasta.** Aceptar una puja, cambiar el líder y mover el cierre ocurre en una sola transacción. Nunca hay dos líderes.
- **RT-02 Idempotencia.** Cada solicitud de puja lleva una clave idempotente; repetirla devuelve el resultado ya registrado.
- **RT-03 Reloj del servidor.** El servidor decide apertura y cierre. Cierre y puja simultáneos se resuelven bajo la misma exclusión.
- **RT-04 Dinero exacto.** Sin floats. Moneda y reglas no cambian si ya hay ofertas, salvo por un procedimiento explícito y auditado.
- **RT-06 Eventos.** Se emiten después del registro confirmado e incluyen ID, versión de estado y hora del servidor.
- **RT-07 Adjudicación única.** Cada cierre produce un certificado con número único. Las correcciones no borran el historial.

**Orden de recepción único** (RF-14). El servidor asigna un orden comparable entre todos los canales (web y WhatsApp). Pujas simultáneas del mismo importe se resuelven por ese orden.

**Contrato de la puja** (§5.4):
- Entrada: subasta, importe, ID de solicitud y origen autenticado.
- Salida: aceptada, rechazada o ya procesada, junto con el ID de la puja, el estado actualizado y el siguiente importe.

**Pendientes (P-04):** se dejan como reglas configurables y se escriben como casos de prueba.
- Primera puja en martillo (RT-05 propone que sea igual al incremento).
- Precio efectivo y competencia entre máximos automáticos y pujas manuales.
- Visibilidad, modificación y cancelación de un máximo.
- Fórmula del alargue: reiniciar 2 minutos o sumar al cierre.
- Efecto de una anulación sobre el líder y sobre una adjudicación ya emitida.

---

## Parte 5 — Tiempo real

**Requisitos:** RF-15, RF-16 · **Invariantes:** RT-06 · **Aceptación:** CA-07

> **Estado: implementado** en `crates/remates-api`: WebSocket por subasta, snapshot más eventos versionados, `resync` y difusión entre instancias leyendo el outbox. Falta la prueba de carga de 12.000 usuarios (Parte 12).

- WebSocket con servidor autoritativo y cuenta regresiva sincronizada con el reloj del servidor.
- Eventos de oferta aceptada, cambio de cierre, cierre de subasta y notificaciones del usuario.
- Reconexión automática con **resincronización completa del estado**. Una oferta rechazada nunca se muestra como aceptada.
- Polling de respaldo si el WebSocket no conecta.
- Sin conexión no se envían pujas. Si la conexión se corta durante una respuesta, se consulta el resultado antes de reintentar, usando la idempotencia.
- Preparado para 12.000 usuarios simultáneos (ver Parte 12).

---

## Parte 6 — Notificaciones web

**Requisitos:** RF-17, RF-18 · **Aceptación:** CA-08 · **Decisiones:** DEC-07, DEC-09

> **Estado: backend implementado.** Los avisos privados (decisión de la puja, oferta superada o máximo superado, adjudicación) llegan por el WebSocket del usuario autenticado. Falta la interfaz web.

- Avisos dentro de la plataforma:
  - confirmación de la oferta propia;
  - nuevas ofertas en subastas donde el usuario participa;
  - oferta superada y máximo superado;
  - últimos minutos y adjudicación.
- **Oferta superada → botón "pujar el siguiente incremento"**, con confirmación y nueva validación del motor.
- El mínimo de v1 son avisos dentro de la plataforma. Correo, push del navegador y push móvil no entran sin definición propia (P-07).

---

## Parte 7 — Conectores de WhatsApp

Son cuatro funciones **separadas e independientes**. La falla de una no afecta a las demás ni a las subastas (RF-25).

### 7a. Identidad
Verificación y 2FA por la API oficial (ver Parte 2, RF-19). No se mezcla con el canal ni con las pujas.

### 7b. Captura de mensajes
**Requisitos:** RF-20 · **Pendiente:** P-01

- Captura los mensajes de la operación y los asocia a un usuario y una subasta.
- Se muestran en el panel de administración para seguimiento.
- Se guarda el ID externo único, el contenido relevante, la recepción, la asociación y el estado de procesamiento.

### 7c. Publicación en el canal
**Requisitos:** RF-21 · **Aceptación:** CA-10 · **Pendiente:** P-05

- Publicación automática en el canal de Paraguay Remates con texto, foto y enlace a la subasta.
- Cola con prioridad que fija el administrador. Límites de base: 30 minutos y hasta 5 publicaciones por día.
- Registro de cada envío y alertas de falla.
- **Antes de implementar:** hacer un relevamiento técnico de la viabilidad del canal. Meta no ofrece automatización oficial de canales, y no se diseña nada para eludir sus controles.

### 7d. Pujas por WhatsApp y confirmación — *nuevo en v3*
**Requisitos:** RF-22 a RF-24 · **Aceptación:** CA-04, CA-09 · **Decisión:** DEC-14 (C) · **Pendiente:** P-01

- Hoy el cliente confirma con 👍 cada puja válida recibida por WhatsApp, en orden de recepción. La plataforma debe hacer lo mismo de forma automática.
- La puja pasa por **el mismo motor** (Parte 4); no existe un libro de ofertas paralelo.
- Responder **solo después** de que el motor decida, con subasta, importe y resultado. Capturar el mensaje no equivale a aceptarlo.
- Propuesta: tres respuestas posibles (aceptada, rechazada, pendiente de identificación), correlacionadas con el mensaje original y con reintentos que no dupliquen ofertas.
- **A resolver antes de construirla:**
  - si el ingreso es automático y estructurado, o asistido por un administrador;
  - identidad del remitente, formato, duplicados y mensajes tardíos;
  - orden frente a las pujas web.

  No se contrata un bot que interprete texto libre.
- Esta parte puede ampliar el esfuerzo presupuestado. Si es así, se documenta el impacto antes de incluirla en el plan firme.

**Diferido (DEC-10):** avisos individuales de "te superaron" por WhatsApp.

---

## Parte 8 — Post-subasta: documentos y cobranza

**Requisitos:** RF-26 a RF-29 · **Aceptación:** CA-11 · **Decisiones:** DEC-11, DEC-13 · **Pendiente:** P-02

- Avisos al ganador, al vendedor y al administrador.
- **Certificado PDF de adjudicación** con número secuencial único (N001, N002…). Certifica a quién se adjudicó la venta; **no** es factura ni comprobante de pago.
- **Comprobante no fiscal**, generado automáticamente para la gestión actual de cobranza. No reemplaza la factura por "servicio publicidad" que hoy emite el cliente.
- Plantilla y reglas de emisión que aporta el cliente: emisor, receptor, concepto, importe, comisiones, numeración y momento de emisión.
- Panel de cobranza con estados pendiente, pagada y vencida.
- El pago y la entrega se registran a mano, sin pasarela. Así se cierra la operación y se habilitan las reseñas.
- **Diferido:** integración con facturación legal o digital.

---

## Parte 9 — Reputación y exportaciones

**Requisitos:** RF-30

- Reseñas entre comprador y vendedor (puntaje y comentario) después del cierre de la operación.
- Perfil público con el promedio y los comentarios recibidos.
- Exportación de cualquier subasta en cualquier momento, con historial completo en CSV y PDF, según permisos.

---

## Parte 10 — Panel de administración

**Requisitos:** RF-31

Consolida las herramientas de operación:
- monitoreo en vivo de las subastas;
- verificación de usuarios y vendedores;
- moderación de publicaciones;
- parámetros (incrementos, alargue, vigencias, límites del canal);
- anulaciones con motivo y cierre de operaciones;
- cobranza, exportaciones y bandeja de mensajes de WhatsApp.

---

## Parte 11 — Difusión

**Requisitos:** RF-32

- Vista previa enriquecida (Open Graph) al compartir en WhatsApp, Facebook y otras redes.
- Espacio para embeber YouTube o Facebook Live en la página de la subasta. Producir o alojar la transmisión queda fuera del alcance.

---

## Parte 12 — Calidad, carga y puesta en producción

**Requisitos:** RF-33 · **Aceptación:** CA-01 a CA-12 · **Pendiente:** P-06

- Cada CA de la v3 se convierte en pruebas automatizadas, con énfasis en concurrencia, cierre y alargue, reconexión e idempotencia.
- **Prueba de carga con 12.000 usuarios simultáneos.** El perfil se acuerda con el cliente:
  - porcentaje de usuarios activos y pujas por segundo;
  - cantidad de subastas, duración y latencias aceptables.
- Runbook de operación, documentación y capacitación de 2 horas.

---

## Modelo de datos mínimo (§5.2)

| Entidad | Datos clave |
|---|---|
| Usuario / identidad | ID, celular normalizado, roles, estado, documento protegido, verificaciones, términos aceptados (versión) |
| Vendedor | usuario, solicitud, acuerdo aceptado, revisión, administrador que aprueba |
| Publicación / subasta | vendedor, ficha, imágenes, moneda, modalidad, base, incremento, visibilidad, apertura, cierre, estado |
| Puja | ID, subasta, usuario, importe, **origen**, **clave idempotente**, **orden del servidor**, fecha, resultado |
| Máximo automático | usuario, subasta, límite protegido, orden de registro, estado |
| Mensaje / evento externo | ID externo único, canal, usuario y subasta, recepción, resultado del motor, estado de respuesta |
| Adjudicación / documento | subasta, ganador, importe, certificado secuencial, comprobante, estado de emisión |
| Operación / reseña | cobranza, pago y entrega registrados, cierre, reseñas |
| Auditoría / notificación | actor, acción, motivo, referencia, fecha, destinatario, intento, resultado, correlación |

---

## Orden de construcción sugerido

Sigue el cronograma de la v2 (unas 10 semanas). La v3 pide adelantar P-01 y P-05.

| Semanas | Partes | Hito |
|---|---|---|
| 1–2 | 1 (base), 2 (identidad y roles); **relevamientos P-01, P-03 y P-05** | Diseño aprobado |
| 3–5 | 3 (catálogo), 4 (motor), 5 (tiempo real) | Demo del motor |
| 6–7 | 6 (avisos web), 7 (WhatsApp), 9 (reputación), 11 (difusión) | Beta |
| 8–9 | 8 (post-subasta), 10 (admin), 12 (carga y correcciones) | Aceptación |
| 10–11 | Producción, capacitación y contingencia | Go-live |

**Pueden empezar ya:** Partes 1, 2 y 3, y el motor (Parte 4) con las reglas pendientes configurables.
**Bloqueadas por decisiones:** 7c (P-05), 7d (P-01), el formato de documentos de la Parte 8 (P-02) y la relación con el ERP (P-03).

---

## Fuera de alcance de v1

- App móvil nativa: viene después de la web, sin fecha ni presupuesto (DEC-01).
- Facturación legal o digital.
- Pagos en línea y depósitos o garantías reembolsables.
- Logística de entrega.
- Analítica de comportamiento más allá del monitoreo operativo.
- Avisos individuales de "te superaron" por WhatsApp.
- Reemplazo o migración del ERP actual (DEC-03): la solución es propia y convive con el ERP hasta que P-03 diga otra cosa.
- Revisión legal y tributaria, y redacción de textos legales: le corresponden al cliente.
- Integración cripto: lo conversado sobre cripto era el pago al proveedor, no un medio de pago de las subastas.

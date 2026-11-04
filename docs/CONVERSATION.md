# Racional: de la propuesta v2 a la especificación técnica v3

Este documento explica **por qué** la especificación de Paraguay Remates pasó de la
propuesta comercial v2 a la especificación técnica revisada v3. Se basa en la conversación
de WhatsApp entre el proveedor ("Tú", Eliseo) y el cliente (Agro Oro / Paraguay Remates),
entre el 20/10/2026 y el 04/11/2026. **Es el racional de lo que construimos en este repo.**

| Documento | Archivo | Rol |
|---|---|---|
| Propuesta v2 (EN), 27/10/2026 | [`Paraguay_Remates_v2_EN.pdf`](./Paraguay_Remates_v2_EN.pdf) | Presupuesto con alcance funcional (S1). Precio, plazos, hitos. |
| Especificación técnica revisada v3, 04/11/2026 | [`Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf`](./Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf) | Consolida v2 + conversación. Requisitos (RF), invariantes del motor (RT), criterios de aceptación (CA) y pendientes (P). **Fuente de verdad para implementar.** |
| Esta conversación | `CONVERSATION.md` | Evidencia y razonamiento (S2) detrás de cada cambio. |

> Nota: la exportación original solo contiene texto. Las notas de voz, imágenes, videos y
> documentos adjuntos (KickOff, condiciones de subasta, ejemplos de factura) **no están
> disponibles**, por lo que algunas decisiones quedan marcadas como pendientes en v3.
> Se omitieron datos personales y de pago (teléfonos, contactos, direcciones de billetera)
> que no aportan al racional técnico.

---

## 1. Resumen: qué cambió y por qué

La v2 era un **presupuesto**: describía qué se iba a entregar y a qué precio, pero no fijaba
las reglas exactas del motor ni distinguía qué estaba aprobado y qué estaba asumido. Después
de enviarla, la conversación siguió y aparecieron decisiones y pedidos nuevos. La v3:

1. **Separa la evidencia de la aprobación.** Cada decisión se clasifica como A (aceptada
   expresamente), B (heredada del presupuesto), C (pedida y comprometida, detalle pendiente),
   D (diferida) o P (pendiente/ambigua). Así no tratamos como acordado lo que solo se asumió.
2. **Consolida "web primero, app después"** (decidido el 02/11).
3. **Agrega la confirmación de pujas recibidas por WhatsApp** (pedido posterior al envío de v2).
4. **Delimita facturación y ERP:** se mantiene la operación actual de facturación y se hace
   una solución propia, sin reemplazar ni escribir sobre el ERP existente por defecto.
5. **Convierte el alcance en algo implementable y comprobable:** requisitos RF-01…RF-33,
   invariantes del motor RT-01…RT-07, criterios de aceptación CA-01…CA-12 y pendientes P-01…P-09.

Precio, hitos y plazo de v2 **no cambian** (USD 4.000; 40/30/30; ~10 semanas; mantenimiento
USD 1.200/mes). Si los pedidos nuevos amplían el esfuerzo, se debe documentar el impacto
antes de sumarlos al plan.

---

## 2. Cronología de la conversación

### 20/10 — Arranque
- El cliente comparte el sitio (Paraguayremates.com), las *Condiciones Subastas 2026* y el
  formulario de inscripción de vendedor particular.
- El proveedor pide el enlace al canal de WhatsApp "para ver el proceso en acción". La
  operación actual de remates corre sobre ese canal: es el punto de partida que hay que digitalizar.

### 22/10–23/10 — Primera documentación y llamada
- El proveedor menciona que hubo "inconvenientes con la integración del canal de WhatsApp",
  con una solución alternativa que funciona. Esto deriva en las **dos soluciones paralelas e
  independientes** de v2 (captura de mensajes + publicación en canal).
- Se comparte un documento de trabajo y se hace una llamada para cerrar definiciones.

### 26/10 — Presupuesto y primeras preguntas
- Se envía el primer presupuesto.
- Pregunta del proveedor: *"¿dos monedas al mismo tiempo en la misma subasta?"* La respuesta
  fue por audio (no disponible). v2 resuelve con **una moneda por publicación (USD o PYG), sin
  conversión** → en v3 queda como **DEC-15 (B/P)**.
- Web vs. app: el proveedor explica que apunta a web porque el procesamiento del backend se
  reutiliza luego para la app, más rápido y barato. Ofrece hacer solo app o ambas si el cliente lo prefiere.

### 27/10 — El día de las definiciones (base de v2 y de casi toda la matriz de v3)

**ERP existente (DEC-03).** El cliente tiene permiso de su otro proveedor para usar y
modificar el ERP actual, y propone compartir base de datos en el hosting. El proveedor
observa que ese ERP está hecho para una operación manual, no automatizada:

> *"Hacer integraciones es difícil porque es restrictivo. Seguramente diseñemos algo nuevo
> desde nuestro lado directamente."* — Cliente: *"Bueno. Estoy de acuerdo."*

→ v3: se acepta la **dirección de una solución nueva**, pero **no** un reemplazo del ERP ni
una migración definida. No se escribe sobre la base actual por defecto (P-03).

**Respuesta punto por punto** a la lista de comentarios del cliente:

| # | Tema | Proveedor | Cliente | En v3 |
|---|---|---|---|---|
| 1 | Verificación | SMS o WhatsApp | "Por WhatsApp me parece bien" | **DEC-04 (A)** – código + 2FA vía WhatsApp Business API oficial (RF-02). |
| 2 | Modalidades | Pide detalle de martillo vs. reserva | Martillo: arranca desde 0, sin base. Reserva: arranca desde el mínimo del vendedor | **DEC-05 (A)** – RF-09. La regla exacta de primera puja queda como propuesta (RT-05, P-04). |
| 3 | Escalonamiento recomendado | Sin problema | ✅ | **DEC-06 (A)** – RF-07; la tabla de rangos debe validarla el cliente. |
| 4 | Avisos de confirmación y nuevas pujas | En el roadmap | ✅ | **DEC-07 (A)** – RF-17. |
| 5 | Caída de conexión | Se reconecta; sin conexión no se puja, salvo pujas preprogramadas | ✅ | **DEC-08 (A)** – RF-15/RF-16; máximos automáticos siguen en el servidor. |
| 6 | Avisos por WhatsApp | ¿Mensajes 1 a 1? Volumen alto, riesgo de spam | Avisar a cada postor superado y ofrecerle pujar +1; explorar WhatsApp porque "el engagement es mucho mayor" | **DEC-09 (A/P)** en la web con botón de pujar el siguiente incremento. **DEC-10 (D)** individual por WhatsApp diferido (costo por mensaje, opt-in, riesgo de baneo). |
| 7 | Facturación | Facturas no legales; pide formato | Hoy facturan como "servicio publicidad"; la cobranza tiene que estar automatizada | **DEC-11 (A/P)** – comprobante no fiscal automático + panel de cobranza; facturación digital/legal diferida (RF-27/28/29, P-02). |
| 8 | Textos legales | Los define el cliente | ✅ | **DEC-12 (A)** – RF-04. |
| 9 | Certificado PDF secuencial | Certifica a quién se adjudica la venta, con N001, N002… | "¿Es solo un número de referencia? Como una factura" | **DEC-13 (B/P)** – RF-26: el certificado **no** es factura ni comprobante; hay que explicar la diferencia. |

Cierre del bloque:

> Proveedor: *"Las facturas las seguimos generando como hasta ahora, de manera automática, y
> más tarde implementamos facturación digital. ¿Hay algún otro punto que creas que necesitamos
> revisar?"* — Cliente: *"Hasta aquí está ok. Por ahora no."*

v3 aclara que este "ok" general **no equivale a aprobar cada interpretación** de un documento
enviado después (la v2 en inglés se envió a las 17:24).

**Pedido nuevo tras enviar v2 (DEC-14, C):**

> Cliente: *"Hoy, cuando alguien puja en WhatsApp, si la puja es válida y aceptada le mandamos
> un 👍 para confirmar (porque muchas veces 2 o 3 clientes mandan la misma puja casi al mismo
> tiempo. Siempre se trata por orden de recepción). ¿Cómo lo haríamos para los clientes que
> pujan por WhatsApp?"*
> Proveedor: *"Podemos mandar un mensaje de confirmación en respuesta a sus pujas."*

Es **el cambio funcional más importante de v3**. v2 solo garantizaba *captura* de mensajes;
ahora hay un compromiso de *confirmar* pujas de WhatsApp. Eso obliga a que:
- la confirmación salga **solo después** de que el motor acepte la puja, nunca al capturar
  el mensaje (RF-22);
- el mensaje se conecte con el **mismo motor** de la web, sin un libro de ofertas paralelo (RF-23, §5.1);
- exista un **orden único** entre web y WhatsApp, porque "el orden de recepción importa" (RF-14, CA-04);
- haya idempotencia: una reentrega del mensaje no duplica la puja (RT-02, CA-09).

Queda pendiente (P-01) si el ingreso es automático estructurado o asistido por un
administrador. No hay evidencia para contratar un bot que interprete texto libre y puje solo.

### 28/10 — Seguimiento
- Intercambio por audio (no disponible). El proveedor invita a plantear cualquier duda. El
  cliente comparte un contacto (probablemente del equipo del ERP actual; relacionado con P-03).

### 02/11 — Web vs. app (DEC-01, DEC-02)
- El cliente pregunta si la app puede estar desde el principio.
- Proveedor: la web es más accesible ("un clic y cualquier teléfono la abre"); una app debe
  ser liviana para teléfonos viejos, compatible con distintas versiones de Android y portarse
  a iOS; eso suma ~2 semanas. Web y app consumen el mismo backend y funcionan juntas.
- Cliente: ellos pensaban en app porque una vez instalada "la gente la tiene a mano".
- Decisión del cliente:

> *"De todas formas, primeramente se desarrolla la web. Entonces avanzamos con el desarrollo
> web ahora, con la mira en después poder pasarlo a una app, una vez que sepamos cómo está todo
> con la web."* — Proveedor: *"Sip."*

→ v3: **web primero (A)**, app futura **sin fecha ni presupuesto (D)**. Las "2 semanas" son
una estimación, no un hito. La arquitectura debe dejar una API compartible con clientes móviles (§5.1, §7).

### 03/11 — Justificación escrita
El proveedor resume: una web es de acceso universal (cuenta, validación, subastas, mails,
notificaciones con un único desarrollo); una app depende de los recursos del dispositivo, que
pueden faltar en equipos viejos o diferir entre sistemas operativos. Ambas usan los mismos
servicios backend.

v3 aclara que la mención de "mails" o "app" no alcanza para exigir correo, push de navegador o
push móvil en v1: el mínimo son **avisos dentro de la plataforma** (RF-18, P-07).

### 04/11 — Inicio del proyecto (DEC-17)
- El cliente pregunta cómo pagar el anticipo; se conversan opciones de pago **del desarrollo**.
- Proveedor: *"Nosotros ya tenemos confirmación de ustedes así que vamos a empezar igual."*

→ v3: evidencia de que el proyecto avanza, **no** prueba de pago ni fecha contractual de inicio.
Se aclara explícitamente que **no se agrega ninguna integración cripto al producto**: era el
pago al proveedor, no un medio de pago de las subastas.

---

## 3. Principios que salen de esta conversación (y que guían el código)

1. **El motor de subastas es la única autoridad.** Web, WhatsApp y una futura app son canales;
   ninguno acepta ni confirma una puja por su cuenta.
2. **Orden de recepción en el servidor.** Pujas simultáneas del mismo importe se resuelven con
   un orden único y atómico por subasta (RT-01). Nunca dos líderes.
3. **Reloj del servidor.** Apertura, cierre y alargue los decide el servidor; el contador del
   cliente es solo una estimación (RT-03).
4. **Idempotencia en todo.** Cortes de conexión y reentregas de WhatsApp no duplican pujas (RT-02).
5. **Dinero exacto, una moneda por subasta.** Sin conversión ni floats (RT-04).
6. **Conectores aislados.** Si falla WhatsApp (captura o canal), las subastas web siguen (RF-25).
7. **Documentos sin pretensión fiscal.** Certificado secuencial ≠ comprobante ≠ factura legal.
8. **Backend preparado para app**, pero la entrega v1 es web adaptable.
9. **Solución nueva junto al ERP**, no encima de él, hasta que el relevamiento diga otra cosa.
10. **No dar por aprobado lo que no lo está.** Las reglas marcadas como pendientes (P-01…P-09)
    se implementan de forma configurable o se cierran con el cliente antes de aceptar el módulo.

## 4. Pendientes heredados de la conversación

Ver la tabla completa en la sección 8 de la v3. Los que más afectan al código:

- **P-01** – circuito de pujas por WhatsApp: origen, ingreso automático o asistido, orden entre
  canales, mensajes tardíos, formato de respuesta.
- **P-04** – reglas del motor: primera puja en martillo, competencia entre máximos automáticos,
  empate entre canales, fórmula del alargue, efecto de anulaciones.
- **P-05** – viabilidad técnica del canal de WhatsApp y sentido del límite de "30 minutos / 5 por día".
- **P-02 / P-03** – formato real de la factura actual y relación con el ERP.
- **P-06** – perfil de carga para la prueba de 12.000 usuarios simultáneos.

Catálogo, roles y la base web pueden avanzar sin esperar estos pendientes.

/* DO NOT EDIT THIS FILE - it is machine generated */
#include <jni.h>
/* Header for class com_risingwave_java_binding_Binding */

#ifndef _Included_com_risingwave_java_binding_Binding
#define _Included_com_risingwave_java_binding_Binding
#ifdef __cplusplus
extern "C" {
#endif
/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    vnodeCount
 * Signature: ()I
 */
JNIEXPORT jint JNICALL Java_com_risingwave_java_binding_Binding_vnodeCount
  (JNIEnv *, jclass);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    hummockIteratorNew
 * Signature: ([B)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_hummockIteratorNew
  (JNIEnv *, jclass, jbyteArray);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    hummockIteratorNext
 * Signature: (J)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_hummockIteratorNext
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    hummockIteratorClose
 * Signature: (J)V
 */
JNIEXPORT void JNICALL Java_com_risingwave_java_binding_Binding_hummockIteratorClose
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetKey
 * Signature: (J)[B
 */
JNIEXPORT jbyteArray JNICALL Java_com_risingwave_java_binding_Binding_rowGetKey
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetOp
 * Signature: (J)I
 */
JNIEXPORT jint JNICALL Java_com_risingwave_java_binding_Binding_rowGetOp
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowIsNull
 * Signature: (JI)Z
 */
JNIEXPORT jboolean JNICALL Java_com_risingwave_java_binding_Binding_rowIsNull
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetInt16Value
 * Signature: (JI)S
 */
JNIEXPORT jshort JNICALL Java_com_risingwave_java_binding_Binding_rowGetInt16Value
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetInt32Value
 * Signature: (JI)I
 */
JNIEXPORT jint JNICALL Java_com_risingwave_java_binding_Binding_rowGetInt32Value
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetInt64Value
 * Signature: (JI)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_rowGetInt64Value
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetFloatValue
 * Signature: (JI)F
 */
JNIEXPORT jfloat JNICALL Java_com_risingwave_java_binding_Binding_rowGetFloatValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetDoubleValue
 * Signature: (JI)D
 */
JNIEXPORT jdouble JNICALL Java_com_risingwave_java_binding_Binding_rowGetDoubleValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetBooleanValue
 * Signature: (JI)Z
 */
JNIEXPORT jboolean JNICALL Java_com_risingwave_java_binding_Binding_rowGetBooleanValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetStringValue
 * Signature: (JI)Ljava/lang/String;
 */
JNIEXPORT jstring JNICALL Java_com_risingwave_java_binding_Binding_rowGetStringValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetTimestampValue
 * Signature: (JI)Ljava/sql/Timestamp;
 */
JNIEXPORT jobject JNICALL Java_com_risingwave_java_binding_Binding_rowGetTimestampValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetDecimalValue
 * Signature: (JI)Ljava/math/BigDecimal;
 */
JNIEXPORT jobject JNICALL Java_com_risingwave_java_binding_Binding_rowGetDecimalValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetTimeValue
 * Signature: (JI)Ljava/sql/Time;
 */
JNIEXPORT jobject JNICALL Java_com_risingwave_java_binding_Binding_rowGetTimeValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetDateValue
 * Signature: (JI)Ljava/sql/Date;
 */
JNIEXPORT jobject JNICALL Java_com_risingwave_java_binding_Binding_rowGetDateValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetIntervalValue
 * Signature: (JI)Ljava/lang/String;
 */
JNIEXPORT jstring JNICALL Java_com_risingwave_java_binding_Binding_rowGetIntervalValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetJsonbValue
 * Signature: (JI)Ljava/lang/String;
 */
JNIEXPORT jstring JNICALL Java_com_risingwave_java_binding_Binding_rowGetJsonbValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetByteaValue
 * Signature: (JI)[B
 */
JNIEXPORT jbyteArray JNICALL Java_com_risingwave_java_binding_Binding_rowGetByteaValue
  (JNIEnv *, jclass, jlong, jint);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowGetArrayValue
 * Signature: (JILjava/lang/Class;)Ljava/lang/Object;
 */
JNIEXPORT jobject JNICALL Java_com_risingwave_java_binding_Binding_rowGetArrayValue
  (JNIEnv *, jclass, jlong, jint, jclass);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    rowClose
 * Signature: (J)V
 */
JNIEXPORT void JNICALL Java_com_risingwave_java_binding_Binding_rowClose
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    streamChunkIteratorNew
 * Signature: ([B)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_streamChunkIteratorNew
  (JNIEnv *, jclass, jbyteArray);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    streamChunkIteratorNext
 * Signature: (J)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_streamChunkIteratorNext
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    streamChunkIteratorClose
 * Signature: (J)V
 */
JNIEXPORT void JNICALL Java_com_risingwave_java_binding_Binding_streamChunkIteratorClose
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    streamChunkIteratorFromPretty
 * Signature: (Ljava/lang/String;)J
 */
JNIEXPORT jlong JNICALL Java_com_risingwave_java_binding_Binding_streamChunkIteratorFromPretty
  (JNIEnv *, jclass, jstring);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    sendCdcSourceMsgToChannel
 * Signature: (J[B)Z
 */
JNIEXPORT jboolean JNICALL Java_com_risingwave_java_binding_Binding_sendCdcSourceMsgToChannel
  (JNIEnv *, jclass, jlong, jbyteArray);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    recvSinkWriterRequestFromChannel
 * Signature: (J)[B
 */
JNIEXPORT jbyteArray JNICALL Java_com_risingwave_java_binding_Binding_recvSinkWriterRequestFromChannel
  (JNIEnv *, jclass, jlong);

/*
 * Class:     com_risingwave_java_binding_Binding
 * Method:    sendSinkWriterResponseToChannel
 * Signature: (J[B)Z
 */
JNIEXPORT jboolean JNICALL Java_com_risingwave_java_binding_Binding_sendSinkWriterResponseToChannel
  (JNIEnv *, jclass, jlong, jbyteArray);

#ifdef __cplusplus
}
#endif
#endif
